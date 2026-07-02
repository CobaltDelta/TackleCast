//! DX12 ↔ CUDA shared buffer infrastructure for zero-copy GPU decode.
//!
//! Creates DX12 committed buffers with `D3D12_HEAP_FLAG_SHARED`, exports
//! NT handles for CUDA import, and wraps them as wgpu buffers for
//! `copy_buffer_to_texture` on the render side.
//!
//! Double-buffered: two sets of Y/U/V plane buffers so the CUDA decode
//! thread can write to one while the renderer reads from the other.

#![cfg(feature = "gpu-decode")]

use std::ffi::c_void;
use tracing::{debug, info, warn};
use wgpu::hal::api::Dx12;
use windows::Win32::Graphics::Direct3D12::*;
use windows::core::PCWSTR;

/// Number of double-buffer sets.
pub const NUM_BUFFER_SETS: usize = 2;

/// A set of Y, U, V plane buffers for one frame.
pub struct PlaneBufferSet {
    pub y_buffer: wgpu::Buffer,
    pub u_buffer: wgpu::Buffer,
    pub v_buffer: wgpu::Buffer,
    pub y_size: u64,
    pub u_size: u64,
    pub v_size: u64,
}

/// Shared handle info needed by the CUDA side to import external memory.
#[derive(Debug, Clone, Copy)]
pub struct SharedPlaneHandles {
    /// NT handle for the Y plane buffer (from CreateSharedHandle).
    pub y_handle: *mut c_void,
    /// NT handle for the U plane buffer.
    pub u_handle: *mut c_void,
    /// NT handle for the V plane buffer.
    pub v_handle: *mut c_void,
    pub y_size: u64,
    pub u_size: u64,
    pub v_size: u64,
    /// Row pitch in bytes (aligned to COPY_BYTES_PER_ROW_ALIGNMENT).
    /// nvJPEG must use these as output pitches so the data layout matches
    /// what wgpu expects in copy_buffer_to_texture.
    pub y_pitch: u32,
    pub uv_pitch: u32,
}

// SAFETY: NT handles are just opaque pointers, safe to send across threads.
unsafe impl Send for SharedPlaneHandles {}
unsafe impl Sync for SharedPlaneHandles {}

/// All shared GPU buffers for the zero-copy pipeline.
pub struct SharedGpuBuffers {
    pub buffer_sets: [PlaneBufferSet; NUM_BUFFER_SETS],
    pub handles: [SharedPlaneHandles; NUM_BUFFER_SETS],
    pub width: u32,
    pub height: u32,
}

/// Calculate buffer sizes for YUV 4:2:2 planes at the given dimensions.
/// Each row is aligned to `COPY_BYTES_PER_ROW_ALIGNMENT` (256 bytes) because
/// wgpu's `copy_buffer_to_texture` requires aligned `bytes_per_row`.
/// Returns (y_row_pitch, uv_row_pitch, y_size, uv_size).
fn plane_sizes(width: u32, height: u32) -> (u32, u32, u64, u64) {
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let y_pitch = (width + align - 1) & !(align - 1);
    let uv_pitch = ((width / 2) + align - 1) & !(align - 1);
    let y_size = (y_pitch as u64) * (height as u64);
    let uv_size = (uv_pitch as u64) * (height as u64);
    (y_pitch, uv_pitch, y_size, uv_size)
}

impl SharedGpuBuffers {
    /// Try to create shared DX12 ↔ CUDA buffers via wgpu's HAL layer.
    /// Returns None if the DX12 backend isn't active or creation fails.
    pub fn try_new(
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> Option<Self> {
        let (y_pitch, uv_pitch, y_size, uv_size) = plane_sizes(width, height);

        info!(
            "creating shared DX12 buffers for zero-copy: {}x{} (Y={}B pitch={}, UV={}B pitch={}, {} sets)",
            width, height, y_size, y_pitch, uv_size, uv_pitch, NUM_BUFFER_SETS
        );

        // Access the raw DX12 device via wgpu HAL.
        // Create all DX12 resources and shared handles inside the callback,
        // then wrap them as wgpu buffers outside.
        let raw_resources = unsafe {
            device.as_hal::<Dx12, _, _>(|hal_device| {
                let hal_device = hal_device?;
                let raw_device: &ID3D12Device = hal_device.raw_device();

                let mut sets = Vec::with_capacity(NUM_BUFFER_SETS);
                for i in 0..NUM_BUFFER_SETS {
                    match create_shared_buffer_set(raw_device, y_size, uv_size, i) {
                        Ok(set) => sets.push(set),
                        Err(e) => {
                            warn!("failed to create shared DX12 buffer set {i}: {e}");
                            return None;
                        }
                    }
                }

                Some(sets)
            })
        };

        let raw_sets = match raw_resources {
            Some(r) => r,
            None => {
                warn!("DX12 HAL not available (wgpu may be using Vulkan) — zero-copy disabled");
                return None;
            }
        };

        // Wrap the DX12 resources as high-level wgpu Buffers.
        let mut buffer_sets_vec = Vec::with_capacity(NUM_BUFFER_SETS);
        let mut handles_vec = Vec::with_capacity(NUM_BUFFER_SETS);

        for raw_set in raw_sets {
            let y_buffer = wrap_as_wgpu_buffer(device, raw_set.y_resource.clone(), y_size);
            let u_buffer = wrap_as_wgpu_buffer(device, raw_set.u_resource.clone(), uv_size);
            let v_buffer = wrap_as_wgpu_buffer(device, raw_set.v_resource.clone(), uv_size);

            buffer_sets_vec.push(PlaneBufferSet {
                y_buffer,
                u_buffer,
                v_buffer,
                y_size,
                u_size: uv_size,
                v_size: uv_size,
            });

            handles_vec.push(SharedPlaneHandles {
                y_handle: raw_set.y_handle,
                u_handle: raw_set.u_handle,
                v_handle: raw_set.v_handle,
                y_size,
                u_size: uv_size,
                v_size: uv_size,
                y_pitch,
                uv_pitch,
            });
        }

        let buffer_sets: [PlaneBufferSet; NUM_BUFFER_SETS] = buffer_sets_vec
            .try_into()
            .ok()
            .expect("buffer_sets length mismatch");
        let handles: [SharedPlaneHandles; NUM_BUFFER_SETS] = handles_vec
            .try_into().expect("handles length mismatch");

        info!("shared DX12 buffers created successfully for zero-copy pipeline");
        Some(Self {
            buffer_sets,
            handles,
            width,
            height,
        })
    }
}

/// Raw DX12 resources for one buffer set, before wrapping as wgpu objects.
struct RawBufferSet {
    y_resource: ID3D12Resource,
    u_resource: ID3D12Resource,
    v_resource: ID3D12Resource,
    y_handle: *mut c_void,
    u_handle: *mut c_void,
    v_handle: *mut c_void,
}

/// Create a set of shared DX12 committed buffers with NT handles.
fn create_shared_buffer_set(
    device: &ID3D12Device,
    y_size: u64,
    uv_size: u64,
    set_index: usize,
) -> Result<RawBufferSet, String> {
    let (y_resource, y_handle) =
        create_shared_committed_buffer(device, y_size, &format!("Y-{set_index}"))?;
    let (u_resource, u_handle) =
        create_shared_committed_buffer(device, uv_size, &format!("U-{set_index}"))?;
    let (v_resource, v_handle) =
        create_shared_committed_buffer(device, uv_size, &format!("V-{set_index}"))?;

    debug!(
        "DX12 shared buffer set {set_index}: Y={y_size}B U={uv_size}B V={uv_size}B"
    );

    Ok(RawBufferSet {
        y_resource,
        u_resource,
        v_resource,
        y_handle,
        u_handle,
        v_handle,
    })
}

/// Create a single DX12 committed buffer with D3D12_HEAP_FLAG_SHARED and
/// export an NT shared handle for CUDA import.
fn create_shared_committed_buffer(
    device: &ID3D12Device,
    size: u64,
    label: &str,
) -> Result<(ID3D12Resource, *mut c_void), String> {
    let heap_properties = D3D12_HEAP_PROPERTIES {
        Type: D3D12_HEAP_TYPE_DEFAULT,
        CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
        MemoryPoolPreference: D3D12_MEMORY_POOL_UNKNOWN,
        CreationNodeMask: 0,
        VisibleNodeMask: 0,
    };

    let resource_desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Alignment: 0,
        Width: size,
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_UNKNOWN,
        SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        Flags: D3D12_RESOURCE_FLAG_NONE,
    };

    let mut resource: Option<ID3D12Resource> = None;

    unsafe {
        device
            .CreateCommittedResource(
                &heap_properties,
                D3D12_HEAP_FLAG_SHARED,
                &resource_desc,
                D3D12_RESOURCE_STATE_COMMON,
                None, // no clear value for buffers
                &mut resource,
            )
            .map_err(|e| {
                format!("CreateCommittedResource failed for {label}: {e}")
            })?;
    }

    let resource = resource
        .ok_or_else(|| format!("CreateCommittedResource returned null for {label}"))?;

    // Create an NT shared handle for CUDA import.
    let handle = unsafe {
        device
            .CreateSharedHandle(&resource, None, windows::Win32::Foundation::GENERIC_ALL.0, PCWSTR::null())
            .map_err(|e| format!("CreateSharedHandle failed for {label}: {e}"))?
    };

    debug!("created shared DX12 buffer '{label}': {size}B, handle={handle:?}");

    Ok((resource, handle.0 as *mut c_void))
}

/// Wrap a raw DX12 ID3D12Resource as a high-level wgpu::Buffer.
fn wrap_as_wgpu_buffer(
    device: &wgpu::Device,
    resource: ID3D12Resource,
    size: u64,
) -> wgpu::Buffer {
    unsafe {
        let hal_buffer =
            wgpu::hal::dx12::Device::buffer_from_raw(resource, size);

        device.create_buffer_from_hal::<Dx12>(
            hal_buffer,
            &wgpu::BufferDescriptor {
                label: Some("tacklecast-shared-plane-buffer"),
                size,
                // COPY_SRC so we can copy_buffer_to_texture
                usage: wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            },
        )
    }
}
