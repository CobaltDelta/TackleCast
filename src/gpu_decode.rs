//! Optional GPU-accelerated MJPEG decode via NVIDIA nvJPEG (CUDA).
//!
//! Dynamically loads nvcuda.dll and nvjpeg64_12.dll at runtime.
//! Returns None/falls back gracefully when CUDA or nvJPEG is unavailable.

#![cfg(feature = "gpu-decode")]

use crate::capture::{CaptureFrame, PixelFormat};
use std::borrow::Cow;
use std::ffi::c_void;
use std::ptr;
use tracing::{debug, info, trace};

// ---------------------------------------------------------------------------
// DHT (Huffman table) injection for UVC MJPEG streams
// ---------------------------------------------------------------------------
//
// Per the USB Video Class spec, UVC devices emit MJPEG frames that omit the
// DHT (Define Huffman Table) segment — the decoder is expected to use the
// standard JPEG Huffman tables. NVIDIA nvJPEG *may* handle this on some
// driver/CUDA versions (e.g. CUDA 13.2 + ShadowCast 3), but other capture
// cards (Elgato, AVerMedia, cheap USB dongles) will fail.
//
// Fix: scan the JPEG for a DHT marker (0xFF 0xC4) before the SOS marker
// (0xFF 0xDA). If absent, splice in the standard tables right before SOS.
// This is a ~420-byte memcpy with negligible overhead.

/// Standard JPEG Huffman tables (2 DC + 2 AC) as a single DHT segment.
/// This is the canonical set from the JPEG specification (ITU-T T.81, Annex K).
#[rustfmt::skip]
static STANDARD_DHT_SEGMENT: &[u8] = &[
    0xFF, 0xC4, // DHT marker
    0x01, 0xA2, // Length = 418 bytes (including this 2-byte length field)

    // --- DC luminance (table 0, class 0) ---
    0x00, // table class 0 (DC), table id 0
    // Number of codes of each length 1..16:
    0x00, 0x01, 0x05, 0x01, 0x01, 0x01, 0x01, 0x01,
    0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // Values:
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
    0x08, 0x09, 0x0A, 0x0B,

    // --- DC chrominance (table 1, class 0) ---
    0x01, // table class 0 (DC), table id 1
    0x00, 0x03, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
    0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
    0x08, 0x09, 0x0A, 0x0B,

    // --- AC luminance (table 0, class 1) ---
    0x10, // table class 1 (AC), table id 0
    0x00, 0x02, 0x01, 0x03, 0x03, 0x02, 0x04, 0x03,
    0x05, 0x05, 0x04, 0x04, 0x00, 0x00, 0x01, 0x7D,
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12,
    0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07,
    0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xA1, 0x08,
    0x23, 0x42, 0xB1, 0xC1, 0x15, 0x52, 0xD1, 0xF0,
    0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0A, 0x16,
    0x17, 0x18, 0x19, 0x1A, 0x25, 0x26, 0x27, 0x28,
    0x29, 0x2A, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39,
    0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49,
    0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59,
    0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79,
    0x7A, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
    0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98,
    0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7,
    0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6,
    0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4, 0xC5,
    0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3, 0xD4,
    0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE1, 0xE2,
    0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA,
    0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8,
    0xF9, 0xFA,

    // --- AC chrominance (table 1, class 1) ---
    0x11, // table class 1 (AC), table id 1
    0x00, 0x02, 0x01, 0x02, 0x04, 0x04, 0x03, 0x04,
    0x07, 0x05, 0x04, 0x04, 0x00, 0x01, 0x02, 0x77,
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21,
    0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71,
    0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91,
    0xA1, 0xB1, 0xC1, 0x09, 0x23, 0x33, 0x52, 0xF0,
    0x15, 0x62, 0x72, 0xD1, 0x0A, 0x16, 0x24, 0x34,
    0xE1, 0x25, 0xF1, 0x17, 0x18, 0x19, 0x1A, 0x26,
    0x27, 0x28, 0x29, 0x2A, 0x35, 0x36, 0x37, 0x38,
    0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48,
    0x49, 0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58,
    0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68,
    0x69, 0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78,
    0x79, 0x7A, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
    0x88, 0x89, 0x8A, 0x92, 0x93, 0x94, 0x95, 0x96,
    0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5,
    0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4,
    0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3,
    0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2,
    0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA,
    0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9,
    0xEA, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8,
    0xF9, 0xFA,
];

/// Ensure JPEG data contains a DHT segment. If missing, inject the standard
/// Huffman tables before the SOS marker. Returns `Cow::Borrowed` when no
/// injection is needed (zero-copy fast path).
fn ensure_dht(jpeg_data: &[u8]) -> Cow<'_, [u8]> {
    // Walk JPEG markers looking for DHT (0xFFC4) or SOS (0xFFDA).
    // We skip the 2-byte SOI and scan marker-by-marker.
    let len = jpeg_data.len();
    if len < 4 {
        return Cow::Borrowed(jpeg_data);
    }

    let mut pos = 2; // skip SOI (FF D8)

    while pos + 1 < len {
        if jpeg_data[pos] != 0xFF {
            // Not at a marker — shouldn't happen in valid JPEG, bail out
            return Cow::Borrowed(jpeg_data);
        }

        let marker = jpeg_data[pos + 1];

        // Found DHT — no injection needed
        if marker == 0xC4 {
            return Cow::Borrowed(jpeg_data);
        }

        // Found SOS without having seen DHT — inject now
        if marker == 0xDA {
            trace!("MJPEG frame missing DHT — injecting standard Huffman tables");
            let mut patched = Vec::with_capacity(jpeg_data.len() + STANDARD_DHT_SEGMENT.len());
            patched.extend_from_slice(&jpeg_data[..pos]);
            patched.extend_from_slice(STANDARD_DHT_SEGMENT);
            patched.extend_from_slice(&jpeg_data[pos..]);
            return Cow::Owned(patched);
        }

        // Skip over this marker's payload.
        // Markers without a length field:
        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) {
            pos += 2;
            continue;
        }

        // Read 2-byte big-endian length (includes the length field itself)
        if pos + 3 >= len {
            break;
        }
        let seg_len = ((jpeg_data[pos + 2] as usize) << 8) | (jpeg_data[pos + 3] as usize);
        pos += 2 + seg_len;
    }

    // Couldn't find SOS — return as-is and let nvJPEG deal with it
    Cow::Borrowed(jpeg_data)
}

// ---------------------------------------------------------------------------
// CUDA Driver API — FFI types and constants
// ---------------------------------------------------------------------------

type CUresult = i32;
type CUdevice = i32;
type CUcontext = *mut c_void;
type CUstream = *mut c_void;
type CUdeviceptr = u64;

const CUDA_SUCCESS: CUresult = 0;

// ---------------------------------------------------------------------------
// nvJPEG API — FFI types and constants
// ---------------------------------------------------------------------------

type NvjpegStatus = i32;
type NvjpegHandle = *mut c_void;
type NvjpegJpegState = *mut c_void;

const NVJPEG_STATUS_SUCCESS: NvjpegStatus = 0;

#[allow(dead_code)]
const NVJPEG_OUTPUT_UNCHANGED: i32 = 0;
const NVJPEG_OUTPUT_YUV: i32 = 1;

const NVJPEG_MAX_COMPONENT: usize = 4;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
enum NvjpegChromaSubsampling {
    Css444 = 0,
    Css422 = 1,
    Css420 = 2,
    Css440 = 3,
    Css411 = 4,
    Css410 = 5,
    CssGray = 6,
    CssUnknown = -1,
}

#[repr(C)]
struct NvjpegImage {
    channel: [*mut u8; NVJPEG_MAX_COMPONENT],
    pitch: [usize; NVJPEG_MAX_COMPONENT],
}

// ---------------------------------------------------------------------------
// Dynamic library wrappers
// ---------------------------------------------------------------------------

struct CudaLib {
    _lib: libloading::Library,
    cu_init: unsafe extern "system" fn(u32) -> CUresult,
    cu_device_get: unsafe extern "system" fn(*mut CUdevice, i32) -> CUresult,
    cu_ctx_create: unsafe extern "system" fn(*mut CUcontext, u32, CUdevice) -> CUresult,
    cu_ctx_destroy: unsafe extern "system" fn(CUcontext) -> CUresult,
    cu_stream_create: unsafe extern "system" fn(*mut CUstream, u32) -> CUresult,
    cu_stream_destroy: unsafe extern "system" fn(CUstream) -> CUresult,
    cu_stream_synchronize: unsafe extern "system" fn(CUstream) -> CUresult,
    cu_mem_alloc: unsafe extern "system" fn(*mut CUdeviceptr, usize) -> CUresult,
    cu_mem_free: unsafe extern "system" fn(CUdeviceptr) -> CUresult,
    cu_memcpy_dtoh: unsafe extern "system" fn(*mut c_void, CUdeviceptr, usize) -> CUresult,
}

impl CudaLib {
    fn try_load() -> Option<Self> {
        let lib = unsafe { libloading::Library::new("nvcuda.dll") }.ok()?;
        unsafe {
            // CUDA driver API uses versioned symbols (e.g. cuCtxCreate_v2)
            let cu_init = *lib.get(b"cuInit\0").ok()?;
            let cu_device_get = *lib.get(b"cuDeviceGet\0").ok()?;
            let cu_ctx_create = *lib.get(b"cuCtxCreate_v2\0").ok()?;
            let cu_ctx_destroy = *lib.get(b"cuCtxDestroy_v2\0").ok()?;
            let cu_stream_create = *lib.get(b"cuStreamCreate\0").ok()?;
            let cu_stream_destroy = *lib.get(b"cuStreamDestroy_v2\0").ok()?;
            let cu_stream_synchronize = *lib.get(b"cuStreamSynchronize\0").ok()?;
            let cu_mem_alloc = *lib.get(b"cuMemAlloc_v2\0").ok()?;
            let cu_mem_free = *lib.get(b"cuMemFree_v2\0").ok()?;
            let cu_memcpy_dtoh = *lib.get(b"cuMemcpyDtoH_v2\0").ok()?;
            Some(Self {
                _lib: lib,
                cu_init,
                cu_device_get,
                cu_ctx_create,
                cu_ctx_destroy,
                cu_stream_create,
                cu_stream_destroy,
                cu_stream_synchronize,
                cu_mem_alloc,
                cu_mem_free,
                cu_memcpy_dtoh,
            })
        }
    }
}

struct NvjpegLib {
    _lib: libloading::Library,
    create_simple: unsafe extern "system" fn(*mut NvjpegHandle) -> NvjpegStatus,
    destroy: unsafe extern "system" fn(NvjpegHandle) -> NvjpegStatus,
    state_create: unsafe extern "system" fn(NvjpegHandle, *mut NvjpegJpegState) -> NvjpegStatus,
    state_destroy: unsafe extern "system" fn(NvjpegJpegState) -> NvjpegStatus,
    get_image_info: unsafe extern "system" fn(
        NvjpegHandle,
        *const u8,
        usize,
        *mut i32,
        *mut i32,
        *mut [i32; NVJPEG_MAX_COMPONENT],
        *mut [i32; NVJPEG_MAX_COMPONENT],
    ) -> NvjpegStatus,
    decode: unsafe extern "system" fn(
        NvjpegHandle,
        NvjpegJpegState,
        *const u8,
        usize,
        i32, // output_format
        *mut NvjpegImage,
        CUstream,
    ) -> NvjpegStatus,
}

impl NvjpegLib {
    fn try_load() -> Option<Self> {
        // Try loading from PATH first (handles bundled DLLs next to exe),
        // then search common CUDA Toolkit install paths.
        let dll_names = ["nvjpeg64_13.dll", "nvjpeg64_12.dll", "nvjpeg64_11.dll"];
        let lib = Self::find_library(&dll_names)?;
        unsafe {
            let create_simple = *lib.get(b"nvjpegCreateSimple\0").ok()?;
            let destroy = *lib.get(b"nvjpegDestroy\0").ok()?;
            let state_create = *lib.get(b"nvjpegJpegStateCreate\0").ok()?;
            let state_destroy = *lib.get(b"nvjpegJpegStateDestroy\0").ok()?;
            let get_image_info = *lib.get(b"nvjpegGetImageInfo\0").ok()?;
            let decode = *lib.get(b"nvjpegDecode\0").ok()?;
            Some(Self {
                _lib: lib,
                create_simple,
                destroy,
                state_create,
                state_destroy,
                get_image_info,
                decode,
            })
        }
    }

    /// Search for nvJPEG DLL by name on PATH, then in common CUDA install directories.
    fn find_library(dll_names: &[&str]) -> Option<libloading::Library> {
        // First try the default search order (exe dir, system dirs, PATH)
        for name in dll_names {
            if let Ok(lib) = unsafe { libloading::Library::new(*name) } {
                info!("loaded {name} from default search path");
                return Some(lib);
            }
        }

        // Search common CUDA Toolkit install locations
        let cuda_base = std::path::Path::new(
            r"C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA",
        );
        if let Ok(entries) = std::fs::read_dir(cuda_base) {
            // Collect and sort version dirs in reverse so newest is tried first
            let mut versions: Vec<_> = entries
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .collect();
            versions.sort_by(|a, b| b.file_name().cmp(&a.file_name()));

            for version_dir in versions {
                // Check both bin/x64/ and bin/ subdirectories
                for sub in &["bin/x64", "bin"] {
                    let bin_dir = version_dir.path().join(sub);
                    for name in dll_names {
                        let full_path = bin_dir.join(name);
                        if full_path.exists() {
                            if let Ok(lib) = unsafe { libloading::Library::new(&full_path) } {
                                info!("loaded {name} from {}", full_path.display());
                                return Some(lib);
                            }
                        }
                    }
                }
            }
        }

        debug!("nvJPEG DLL not found in any searched location");
        None
    }
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum GpuDecodeError {
    Cuda(&'static str, CUresult),
    Nvjpeg(&'static str, NvjpegStatus),
    InvalidData(String),
}

impl std::fmt::Display for GpuDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cuda(op, code) => write!(f, "CUDA {op} failed (error {code})"),
            Self::Nvjpeg(op, code) => write!(f, "nvJPEG {op} failed (error {code})"),
            Self::InvalidData(msg) => write!(f, "invalid JPEG data: {msg}"),
        }
    }
}

// ---------------------------------------------------------------------------
// GPU device memory helper
// ---------------------------------------------------------------------------

struct DeviceBuffer {
    ptr: CUdeviceptr,
}

// ---------------------------------------------------------------------------
// NvjpegDecoder — the public API
// ---------------------------------------------------------------------------

pub struct NvjpegDecoder {
    cuda: CudaLib,
    nvjpeg: NvjpegLib,
    ctx: CUcontext,
    stream: CUstream,
    handle: NvjpegHandle,
    state: NvjpegJpegState,
    // Pre-allocated device buffers for Y, U, V planes
    d_y: DeviceBuffer,
    d_u: DeviceBuffer,
    d_v: DeviceBuffer,
    // Pre-allocated host buffers for readback
    h_y: Vec<u8>,
    h_u: Vec<u8>,
    h_v: Vec<u8>,
    // Current allocation dimensions
    alloc_width: u32,
    alloc_height: u32,
    // Whether first frame has been validated
    validated: bool,
}

impl NvjpegDecoder {
    /// Attempt to initialize CUDA + nvJPEG. Returns None if unavailable.
    pub fn try_new() -> Option<Self> {
        let cuda = CudaLib::try_load().or_else(|| {
            debug!("nvcuda.dll not found — GPU decode unavailable");
            None
        })?;

        let nvjpeg = NvjpegLib::try_load().or_else(|| {
            debug!("nvjpeg DLL not found — GPU decode unavailable");
            None
        })?;

        unsafe {
            // Initialize CUDA driver
            let res = (cuda.cu_init)(0);
            if res != CUDA_SUCCESS {
                debug!("cuInit failed (error {res})");
                return None;
            }

            // Get device 0
            let mut device: CUdevice = 0;
            let res = (cuda.cu_device_get)(&mut device, 0);
            if res != CUDA_SUCCESS {
                debug!("cuDeviceGet failed (error {res})");
                return None;
            }

            // Create context (flags=0 for default scheduling)
            let mut ctx: CUcontext = ptr::null_mut();
            let res = (cuda.cu_ctx_create)(&mut ctx, 0, device);
            if res != CUDA_SUCCESS {
                debug!("cuCtxCreate failed (error {res})");
                return None;
            }

            // Create stream (flags=0 for default)
            let mut stream: CUstream = ptr::null_mut();
            let res = (cuda.cu_stream_create)(&mut stream, 0);
            if res != CUDA_SUCCESS {
                debug!("cuStreamCreate failed (error {res})");
                (cuda.cu_ctx_destroy)(ctx);
                return None;
            }

            // Create nvJPEG handle and state
            let mut handle: NvjpegHandle = ptr::null_mut();
            let res = (nvjpeg.create_simple)(&mut handle);
            if res != NVJPEG_STATUS_SUCCESS {
                debug!("nvjpegCreateSimple failed (error {res})");
                (cuda.cu_stream_destroy)(stream);
                (cuda.cu_ctx_destroy)(ctx);
                return None;
            }

            let mut state: NvjpegJpegState = ptr::null_mut();
            let res = (nvjpeg.state_create)(handle, &mut state);
            if res != NVJPEG_STATUS_SUCCESS {
                debug!("nvjpegJpegStateCreate failed (error {res})");
                (nvjpeg.destroy)(handle);
                (cuda.cu_stream_destroy)(stream);
                (cuda.cu_ctx_destroy)(ctx);
                return None;
            }

            // Pre-allocate buffers for 2560x1440 YUV 4:2:2
            let w = 2560_u32;
            let h = 1440_u32;
            let y_size = (w * h) as usize;
            let uv_size = ((w / 2) * h) as usize;

            let alloc_buf = |size: usize| -> Option<DeviceBuffer> {
                let mut ptr: CUdeviceptr = 0;
                let res = (cuda.cu_mem_alloc)(&mut ptr, size);
                if res != CUDA_SUCCESS {
                    debug!("cuMemAlloc failed for {size} bytes (error {res})");
                    None
                } else {
                    Some(DeviceBuffer { ptr })
                }
            };

            let d_y = alloc_buf(y_size)?;
            let d_u = match alloc_buf(uv_size) {
                Some(buf) => buf,
                None => {
                    (cuda.cu_mem_free)(d_y.ptr);
                    (nvjpeg.state_destroy)(state);
                    (nvjpeg.destroy)(handle);
                    (cuda.cu_stream_destroy)(stream);
                    (cuda.cu_ctx_destroy)(ctx);
                    return None;
                }
            };
            let d_v = match alloc_buf(uv_size) {
                Some(buf) => buf,
                None => {
                    (cuda.cu_mem_free)(d_u.ptr);
                    (cuda.cu_mem_free)(d_y.ptr);
                    (nvjpeg.state_destroy)(state);
                    (nvjpeg.destroy)(handle);
                    (cuda.cu_stream_destroy)(stream);
                    (cuda.cu_ctx_destroy)(ctx);
                    return None;
                }
            };

            info!(
                "nvJPEG GPU decoder initialized (pre-allocated for {}x{} YUV 4:2:2)",
                w, h
            );

            Some(Self {
                cuda,
                nvjpeg,
                ctx,
                stream,
                handle,
                state,
                d_y,
                d_u,
                d_v,
                h_y: vec![0u8; y_size],
                h_u: vec![0u8; uv_size],
                h_v: vec![0u8; uv_size],
                alloc_width: w,
                alloc_height: h,
                validated: false,
            })
        }
    }

    /// Decode a raw JPEG frame on the GPU and return a CaptureFrame.
    pub fn decode(&mut self, jpeg_data: &[u8]) -> Result<CaptureFrame, GpuDecodeError> {
        if jpeg_data.len() < 2 || jpeg_data[0] != 0xFF || jpeg_data[1] != 0xD8 {
            return Err(GpuDecodeError::InvalidData(
                "data does not start with JPEG SOI marker (FF D8)".into(),
            ));
        }

        // Inject standard Huffman tables if the UVC device omitted them.
        // Returns a zero-copy borrow when DHT is already present.
        let jpeg_data = ensure_dht(jpeg_data);

        // Query image info on first frame or if not yet validated
        let (width, height) = if !self.validated {
            let info = self.get_image_info(&jpeg_data)?;
            info!(
                "nvJPEG first frame: {}x{}, subsampling={:?}, components={}",
                info.0, info.1, info.2, info.3
            );
            self.validated = true;
            (info.0, info.1)
        } else {
            // Trust dimensions are stable after first frame
            (self.alloc_width, self.alloc_height)
        };

        // Reallocate if dimensions changed
        self.ensure_buffers(width, height)?;

        // Set up output image descriptor
        let mut output = NvjpegImage {
            channel: [
                self.d_y.ptr as *mut u8,
                self.d_u.ptr as *mut u8,
                self.d_v.ptr as *mut u8,
                ptr::null_mut(),
            ],
            pitch: [
                width as usize,
                (width / 2) as usize,
                (width / 2) as usize,
                0,
            ],
        };

        // Decode on GPU (async on CUDA stream)
        unsafe {
            let res = (self.nvjpeg.decode)(
                self.handle,
                self.state,
                (*jpeg_data).as_ptr(),
                (*jpeg_data).len(),
                NVJPEG_OUTPUT_YUV,
                &mut output,
                self.stream,
            );
            if res != NVJPEG_STATUS_SUCCESS {
                return Err(GpuDecodeError::Nvjpeg("nvjpegDecode", res));
            }

            // Synchronize — wait for GPU decode to finish
            let res = (self.cuda.cu_stream_synchronize)(self.stream);
            if res != CUDA_SUCCESS {
                return Err(GpuDecodeError::Cuda("cuStreamSynchronize", res));
            }
        }

        // Copy decoded planes from device to host
        let y_size = (width * height) as usize;
        let uv_size = ((width / 2) * height) as usize;

        unsafe {
            let res = (self.cuda.cu_memcpy_dtoh)(
                self.h_y.as_mut_ptr() as *mut c_void,
                self.d_y.ptr,
                y_size,
            );
            if res != CUDA_SUCCESS {
                return Err(GpuDecodeError::Cuda("cuMemcpyDtoH (Y)", res));
            }

            let res = (self.cuda.cu_memcpy_dtoh)(
                self.h_u.as_mut_ptr() as *mut c_void,
                self.d_u.ptr,
                uv_size,
            );
            if res != CUDA_SUCCESS {
                return Err(GpuDecodeError::Cuda("cuMemcpyDtoH (U)", res));
            }

            let res = (self.cuda.cu_memcpy_dtoh)(
                self.h_v.as_mut_ptr() as *mut c_void,
                self.d_v.ptr,
                uv_size,
            );
            if res != CUDA_SUCCESS {
                return Err(GpuDecodeError::Cuda("cuMemcpyDtoH (V)", res));
            }
        }

        Ok(CaptureFrame {
            width,
            height,
            format: PixelFormat::Yuvj422p,
            y_data: self.h_y[..y_size].to_vec(),
            u_data: self.h_u[..uv_size].to_vec(),
            v_data: self.h_v[..uv_size].to_vec(),
        })
    }

    /// Query JPEG dimensions and subsampling from compressed data.
    fn get_image_info(
        &self,
        jpeg_data: &[u8],
    ) -> Result<(u32, u32, NvjpegChromaSubsampling, i32), GpuDecodeError> {
        let mut n_components: i32 = 0;
        let mut subsampling: i32 = -1;
        let mut widths = [0i32; NVJPEG_MAX_COMPONENT];
        let mut heights = [0i32; NVJPEG_MAX_COMPONENT];

        unsafe {
            let res = (self.nvjpeg.get_image_info)(
                self.handle,
                jpeg_data.as_ptr(),
                jpeg_data.len(),
                &mut n_components,
                &mut subsampling,
                &mut widths,
                &mut heights,
            );
            if res != NVJPEG_STATUS_SUCCESS {
                return Err(GpuDecodeError::Nvjpeg("nvjpegGetImageInfo", res));
            }
        }

        let css = match subsampling {
            0 => NvjpegChromaSubsampling::Css444,
            1 => NvjpegChromaSubsampling::Css422,
            2 => NvjpegChromaSubsampling::Css420,
            3 => NvjpegChromaSubsampling::Css440,
            4 => NvjpegChromaSubsampling::Css411,
            5 => NvjpegChromaSubsampling::Css410,
            6 => NvjpegChromaSubsampling::CssGray,
            _ => NvjpegChromaSubsampling::CssUnknown,
        };

        Ok((widths[0] as u32, heights[0] as u32, css, n_components))
    }

    /// Reallocate device and host buffers if dimensions changed.
    fn ensure_buffers(&mut self, width: u32, height: u32) -> Result<(), GpuDecodeError> {
        if width == self.alloc_width && height == self.alloc_height {
            return Ok(());
        }

        info!(
            "nvJPEG reallocating buffers: {}x{} -> {}x{}",
            self.alloc_width, self.alloc_height, width, height
        );

        let y_size = (width * height) as usize;
        let uv_size = ((width / 2) * height) as usize;

        // Free old device buffers
        unsafe {
            (self.cuda.cu_mem_free)(self.d_y.ptr);
            (self.cuda.cu_mem_free)(self.d_u.ptr);
            (self.cuda.cu_mem_free)(self.d_v.ptr);
        }

        // Allocate new device buffers
        let alloc = |size: usize, name: &'static str| -> Result<DeviceBuffer, GpuDecodeError> {
            let mut ptr: CUdeviceptr = 0;
            let res = unsafe { (self.cuda.cu_mem_alloc)(&mut ptr, size) };
            if res != CUDA_SUCCESS {
                Err(GpuDecodeError::Cuda(name, res))
            } else {
                Ok(DeviceBuffer { ptr })
            }
        };

        self.d_y = alloc(y_size, "cuMemAlloc (Y)")?;
        self.d_u = alloc(uv_size, "cuMemAlloc (U)")?;
        self.d_v = alloc(uv_size, "cuMemAlloc (V)")?;

        // Resize host buffers
        self.h_y.resize(y_size, 0);
        self.h_u.resize(uv_size, 0);
        self.h_v.resize(uv_size, 0);

        self.alloc_width = width;
        self.alloc_height = height;

        Ok(())
    }
}

impl Drop for NvjpegDecoder {
    fn drop(&mut self) {
        unsafe {
            // Free device memory
            (self.cuda.cu_mem_free)(self.d_y.ptr);
            (self.cuda.cu_mem_free)(self.d_u.ptr);
            (self.cuda.cu_mem_free)(self.d_v.ptr);

            // Destroy nvJPEG resources
            (self.nvjpeg.state_destroy)(self.state);
            (self.nvjpeg.destroy)(self.handle);

            // Destroy CUDA resources
            (self.cuda.cu_stream_destroy)(self.stream);
            (self.cuda.cu_ctx_destroy)(self.ctx);
        }
        info!("nvJPEG GPU decoder destroyed");
    }
}
