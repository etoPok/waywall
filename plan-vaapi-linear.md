# Plan: Forzar DRM_FORMAT_MOD_LINEAR en superficies VAAPI

## Problema actual

Hyprland solo anuncia soporte para NV12 con `DRM_FORMAT_MOD_INVALID`
(modifier `0x00FFFFFFFFFFFFFF`). El buffer VAAPI se exporta con modifier
Broadcom tiled (`0x0300000000606014`). El compositor acepta el buffer
(create_immed succeeds) pero no sabe de-tilear el UV correctamente →
tinte rojo translúcido sobre la imagen.

## Objetivo

Forzar que las superficies VAAPI se allocaten con `DRM_FORMAT_MOD_LINEAR`
para que el compositor pueda interpretarlas correctamente, manteniendo
decodificación por hardware (sin copies CPU).

## Por qué no se puede hacer desde la API pública de FFmpeg

- `av_hwframe_map` no acepta parámetros de modifier
- `AVHWFramesContext` no tiene campos de modifier/tiling
- `AVVAAPIFramesContext.attributes` se pasa a `vaCreateSurfaces` pero
  FFmpeg no maneja `VASurfaceAttribDRMFormatModifiers`

Sin embargo, FFmpeg **sí** pasa cualquier atributo que el usuario
coloque en `AVVAAPIFramesContext.attributes` directamente a
`vaCreateSurfaces`. Esto permite inyectar el modifier sin parchear FFmpeg.

## Por qué ffmpeg-sys-next no expone los tipos VAAPI

El feature `build-vaapi` no está habilitado en `Cargo.toml`. Sin él,
FFmpeg se compila sin VAAPI y bindgen no genera los tipos de
`hwcontext_vaapi.h`. Hay que definirlos manualmente via FFI.

## Cambios necesarios

### Archivo nuevo: `src/vaapi_attr.rs`

Definiciones FFI de las estructuras de libva ausentes en los bindings:

```rust
use std::ffi::c_void;

pub type VASurfaceID = u32;

// VAGenericValueType
pub const VA_GENERIC_VALUE_TYPE_INTEGER: i32 = 1;
pub const VA_GENERIC_VALUE_TYPE_POINTER: i32 = 3;

// VAGenericValue (va/va.h:1662)
#[repr(C)]
#[derive(Copy, Clone)]
pub union VAGenericValueInner {
    pub i: i32,
    pub p: *mut c_void,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct VAGenericValue {
    pub type_: i32,
    pub _pad: [u8; 4],
    pub value: VAGenericValueInner,
}

// VASurfaceAttribType
pub const VA_SURFACE_ATTRIB_PIXEL_FORMAT: i32 = 1;
pub const VA_SURFACE_ATTRIB_DRM_FORMAT_MODIFIERS: i32 = 9;

// VASurfaceAttrib flags
pub const VA_SURFACE_ATTRIB_SETTABLE: u32 = 0x00000002;

// VASurfaceAttrib (va/va.h:1742)
#[repr(C)]
#[derive(Copy, Clone)]
pub struct VASurfaceAttrib {
    pub type_: i32,
    pub flags: u32,
    pub value: VAGenericValue,
}

// VADRMFormatModifierList (va/va_drmcommon.h:226)
#[repr(C)]
#[derive(Copy, Clone)]
pub struct VADRMFormatModifierList {
    pub num_modifiers: u32,
    pub _pad: [u8; 4],
    pub modifiers: *mut u64,
}

// AVVAAPIFramesContext (hwcontext_vaapi.h:88)
#[repr(C)]
#[derive(Copy, Clone)]
pub struct AVVAAPIFramesContext {
    pub attributes: *mut VASurfaceAttrib,
    pub nb_attributes: i32,
    pub _pad: [u8; 4],
    pub surface_ids: *mut VASurfaceID,
    pub nb_surfaces: i32,
}

pub const DRM_FORMAT_MOD_LINEAR: u64 = 0;
```

### Archivo modificar: `src/decoder.rs`

En `try_init_vaapi_hw`, después de configurar `AVHWFramesContext` y
antes de `av_hwframe_ctx_init`, inyectar dos atributos:

1. `VASurfaceAttribPixelFormat` = NV12 (`0x3231564E`)
2. `VASurfaceAttribDRMFormatModifiers` = `[DRM_FORMAT_MOD_LINEAR]`

Código completo a insertar entre el setup de frames_ctx y
av_hwframe_ctx_init:

```rust
use crate::vaapi_attr::*;

// --- Forzar LINEAR ---
let vaapi_ctx = (*frames_ctx).hwctx as *mut AVVAAPIFramesContext;

let mut linear_mod: u64 = DRM_FORMAT_MOD_LINEAR;
let mut mod_list = VADRMFormatModifierList {
    num_modifiers: 1,
    modifiers: &mut linear_mod,
};

let nv12_fourcc: i32 = 0x3231564E;

let attribs: [VASurfaceAttrib; 2] = [
    VASurfaceAttrib {
        type_: VA_SURFACE_ATTRIB_PIXEL_FORMAT,
        flags: VA_SURFACE_ATTRIB_SETTABLE,
        value: VAGenericValue {
            type_: VA_GENERIC_VALUE_TYPE_INTEGER,
            _pad: [0; 4],
            value: VAGenericValueInner { i: nv12_fourcc },
        },
    },
    VASurfaceAttrib {
        type_: VA_SURFACE_ATTRIB_DRM_FORMAT_MODIFIERS,
        flags: VA_SURFACE_ATTRIB_SETTABLE,
        value: VAGenericValue {
            type_: VA_GENERIC_VALUE_TYPE_POINTER,
            _pad: [0; 4],
            value: VAGenericValueInner {
                p: &mut mod_list as *mut VADRMFormatModifierList as *mut c_void,
            },
        },
    },
];

let attribs_ptr = libc::calloc(
    2,
    std::mem::size_of::<VASurfaceAttrib>()
) as *mut VASurfaceAttrib;
std::ptr::copy_nonoverlapping(attribs.as_ptr(), attribs_ptr, 2);

(*vaapi_ctx).attributes = attribs_ptr;
(*vaapi_ctx).nb_attributes = 2;
```

Después de `av_hwframe_ctx_init`, liberar los atributos
(ya no se necesitan después de crear las superficies):

```rust
libc::free((*vaapi_ctx).attributes as *mut c_void);
(*vaapi_ctx).attributes = ptr::null_mut();
(*vaapi_ctx).nb_attributes = 0;
```

### Archivo modificar: `src/main.rs`

Agregar el módulo:

```rust
mod vaapi_attr;
```

## Flujo completo después del cambio

```
avcodec_get_hw_frames_parameters()
  → AVHWFramesContext: sw_format=NV12, 1920x1088, pool=0

Inyectamos attribs:
  → attrib[0] = PixelFormat NV12
  → attrib[1] = DRMFormatModifiers = [LINEAR]

av_hwframe_ctx_init()
  → FFmpeg llama vaCreateSurfaces() con nuestros attribs
  → Driver V3D crea superficies LINEALES

Decoder decodifica → superficies lineales
av_hwframe_map() → modifier = LINEAR
create_immed(LINEAR) → Hyprland acepta ✓
```

## Riesgos

1. **Driver no soporta LINEAR para NV12**: `vaCreateSurfaces` falla,
   `av_hwframe_ctx_init` retorna error. Actualmente el código hace
   `return false` en ese caso → se usa fallback software.

2. **Performance**: Superficies lineales usan más bandwidth de memoria
   que tiled. Para video playback 1080p debería ser aceptable.

3. **Layout de structs FFI**: Los offsets/padding de las structs
   definidas manualmente deben coincidir exactamente con los de C.
   Verificar con `std::mem::size_of` en un test si hay dudas.

## Verificación

1. Ejecutar el programa con `RUST_LOG=info`
2. Confirmar en logs de `avcodec_get_hw_frames_parameters` que
   initial_pool_size=0 (path de atributos ya existentes)
3. El trace Wayland debe mostrar `modifier_hi=0, modifier_lo=0` en
   ambos `params.add()`
4. La imagen debe mostrarse sin tinte rojo

## Alternativa si LINEAR no funciona

Si el driver V3D rechaza LINEAR para NV12, la alternativa es usar
EGL para renderizar la superficie VAAPI como textura (el GPU hace
el de-tiling internamente). Esto requiere:
- `EGL_EXT_image_dma_buf_import`
- Crear `EGLImage` desde el DMA-BUF VAAPI
- Renderizar a `wl_egl_window` via OpenGL ES
