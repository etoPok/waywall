# Diagnóstico: imagen enrojecida con AMD VAAPI

## Observaciones

- El buffer se importa correctamente (sin error de protocolo)
- La imagen es reconocible pero con tinte rojizo
- Ya se probaron modifiers `LINEAR` e `INVALID` → mismo resultado

## Datos del trace AMD

```
layer[0] format 538982482  → R8 (Y plane)
layer[1] format 943215175  → GR88 (UV interleaved)
fd 9:  Y plane,  offset 0,      stride 2048
fd 10: UV plane, offset 2621440, stride 2048
modifier: 0x0200000000000901 (AMD GFX9 tiled)
create_immed: 1920×1080, NV12, flags=0
```

## Hipótesis principal: offset de UV incorrecto

El offset `2621440 = 2048 × 1280` para la UV plane está calculado como si
Y y UV compartieran un solo buffer (single-buffer layout). Pero el
descriptor exporta **dos FDs separados** (fd 9, fd 10) vía
`VA_EXPORT_SURFACE_SEPARATE_LAYERS`.

Si fd 10 es un buffer independiente solo para UV (~1 MB para 2048×540),
el offset `2621440` está **fuera del buffer** → el compositor lee chroma
corrupta → tinte rojizo.

La corrección sería usar **offset 0** para cada plano cuando tiene su
propio FD.

## Pasos para confirmar

### 1. Agregar log de `obj.size` en `DrmFrame::map()`

En `src/drm_frame.rs`, dentro del loop de objetos:

```rust
info!(
    "object[{}]: fd={}, size={}, modifier={:#x}",
    obj_idx, obj.fd, obj.size, obj.format_modifier
);
```

- Si `object[1].size` es ~1 MB → buffer separado → offset debe ser 0
- Si `object[1].size` es ~3.3 MB → buffer compartido → offset es correcto

### 2. Probar offset 0 para el plano UV

En `src/drm_frame.rs`, forzar offset a 0 cuando el FD difiere del primer
plano:

```rust
// Si el FD es distinto al del primer plano, offset debe ser 0
let is_separate_fd = if let Some(first_plane) = layers.first()
    .and_then(|l| l.planes.first())
{
    obj.fd != first_plane.fd
} else {
    false
};

let offset = if is_separate_fd { 0 } else { plane.offset as u32 };
```

### 3. Probar altura 1088 (hw surface real)

El HW surface del decoder VAAPI es 1920×1088, no 1920×1080. Cambiar
`create_immed` a:

```rust
drm_frame.height,  // actual: 1080
// vs
1088,  // hw surface real
```

## Hipótesis alternativa: orden U/V invertido

Si el offset es correcto pero el color sigue siendo rojizo, podría ser que
los canales U (Cb) y V (Cr) están intercambiados. El layer[1] reporta
`GR88` (U byte0, V byte1) pero quizás el compositor interpreta los bytes
al revés (V byte0, U byte1).

Probar: convertir el buffer descargando a RAM con `av_hwframe_transfer_data`
y verificar el orden de los bytes UV manualmente. O probar con format
`NV21` (`0x3132564E`) en `create_immed` si el compositor lo soporta.
