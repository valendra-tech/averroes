# Subtle UI Animations Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Añadir microanimaciones coherentes a controles, estados, contenido efímero y texto del asistente mientras llega por streaming.

**Architecture:** Centralizar duraciones y wrappers de `with_animation` en un módulo pequeño de `crates/gpui/src/ui/animation.rs`. Mantener las claves de elementos estables; para streaming, animar solo la última línea no vacía y no reiniciar la animación por cada delta. Integrar los helpers en los puntos de render existentes sin reestructurar el `AverroesApp` monolítico.

**Tech Stack:** Rust, GPUI (`Animation`, `AnimationExt`, `ease_out_quint`), gpui-component, cargo tests.

---

## Mapa de archivos

- Create: `crates/gpui/src/ui/animation.rs` — duraciones y helpers de fade/entrada de Div.
- Modify: `crates/gpui/src/ui/mod.rs` — exportar el módulo compartido.
- Modify: `crates/gpui/src/ui/markdown.rs` — entrada sutil de la última línea durante streaming.
- Modify: `crates/gpui/src/app.rs` — aplicar animaciones a adjuntos, composer, estados y mensajes; pasar IDs estables al renderer de streaming.
- Test: `crates/gpui/src/ui/markdown.rs` — pruebas puras de selección de la línea animada.

La API de botones existente ya tiene estados hover estáticos y no se cambiará su contrato. Las mejoras de movimiento se aplicarán a sus iconos/contenidos y a transiciones de estado, donde GPUI permite conservar IDs y evitar trabajo por cada frame.

### Task 1: Crear la capa común de animación

**Files:**
- Create: `crates/gpui/src/ui/animation.rs`
- Modify: `crates/gpui/src/ui/mod.rs`

- [ ] **Step 1: Escribir los tests de las constantes y del rango de opacidad**

Añadir en `animation.rs` un módulo de tests para fijar los valores aprobados y la función pura usada por los fades:

```rust
#[cfg(test)]
mod tests {
    use super::{fade_opacity, ATTACHMENT_FADE_DURATION, STREAM_LINE_FADE_DURATION};
    use std::time::Duration;

    #[test]
    fn uses_short_durations_for_subtle_content_motion() {
        assert_eq!(ATTACHMENT_FADE_DURATION, Duration::from_millis(180));
        assert_eq!(STREAM_LINE_FADE_DURATION, Duration::from_millis(140));
    }

    #[test]
    fn fade_opacity_stays_between_start_and_end() {
        assert_eq!(fade_opacity(0.0), 0.0);
        assert_eq!(fade_opacity(1.0), 1.0);
        assert!((0.0..=1.0).contains(&fade_opacity(0.5)));
    }
}
```

- [ ] **Step 2: Ejecutar el test nuevo y comprobar que falla porque el módulo no existe**

Run: `cargo test -p averroes-gpui ui::animation::tests -- --nocapture`

Expected: FAIL because `crates/gpui/src/ui/animation.rs` and its exported symbols do not exist yet.

- [ ] **Step 3: Implementar los helpers mínimos**

Crear el módulo con la API concreta:

```rust
use gpui::{ease_out_quint, Animation, AnimationElement, AnimationExt, Div, ElementId, Styled};
use std::time::Duration;

pub const MESSAGE_FADE_DURATION: Duration = Duration::from_millis(220);
pub const ATTACHMENT_FADE_DURATION: Duration = Duration::from_millis(180);
pub const STREAM_LINE_FADE_DURATION: Duration = Duration::from_millis(140);
pub const STATE_FADE_DURATION: Duration = Duration::from_millis(160);

pub fn fade_opacity(delta: f32) -> f32 {
    delta.clamp(0.0, 1.0)
}

pub fn fade_in(element: Div, id: impl Into<ElementId>, duration: Duration) -> AnimationElement<Div> {
    element.with_animation(
        id,
        Animation::new(duration).with_easing(ease_out_quint()),
        |element, delta| element.opacity(fade_opacity(delta)),
    )
}
```

GPUI respeta automáticamente `App::reduce_motion` en `AnimationExt`, por lo que estos helpers no deben introducir una segunda preferencia de accesibilidad.

- [ ] **Step 4: Exportar el módulo y ejecutar sus tests**

En `crates/gpui/src/ui/mod.rs` añadir `pub mod animation;`. Ejecutar:

Run: `cargo test -p averroes-gpui ui::animation::tests -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Commitar la capa común**

```bash
git add crates/gpui/src/ui/animation.rs crates/gpui/src/ui/mod.rs
git commit -m "feat: add shared UI animation helpers"
```

### Task 2: Animar la última línea de texto en streaming

**Files:**
- Modify: `crates/gpui/src/ui/markdown.rs`
- Test: `crates/gpui/src/ui/markdown.rs`

- [ ] **Step 1: Escribir una prueba pura para identificar la línea final visible**

Añadir una función privada `last_non_empty_line_index` y probar estos casos:

```rust
#[test]
fn streaming_animation_targets_only_the_last_non_empty_line() {
    assert_eq!(last_non_empty_line_index("one\ntwo"), Some(1));
    assert_eq!(last_non_empty_line_index("one\ntwo\n"), Some(1));
    assert_eq!(last_non_empty_line_index("\n\n"), None);
}
```

- [ ] **Step 2: Ejecutar la prueba para confirmar el fallo**

Run: `cargo test -p averroes-gpui ui::markdown::tests::streaming_animation_targets_only_the_last_non_empty_line -- --nocapture`

Expected: FAIL because the helper does not exist.

- [ ] **Step 3: Cambiar el renderer de streaming sin animar cada delta**

Cambiar la firma a `render_streaming_markdown(theme, content, animation_id)` y construir cada línea con `enumerate`. Solo la última línea no vacía se envolverá con:

```rust
fade_in(
    line_element,
    format!("{animation_id}-line-{line_index}"),
    STREAM_LINE_FADE_DURATION,
)
```

Las líneas anteriores conservarán su tipo y contenido actuales. La clave usa el índice de línea, no la longitud del texto, para que añadir tokens en la misma línea no reinicie la animación cada 32 ms; solo una nueva línea activa obtiene una clave nueva.

- [ ] **Step 4: Actualizar el test y ejecutar toda la suite de markdown**

Run: `cargo test -p averroes-gpui ui::markdown::tests -- --nocapture`

Expected: PASS, incluyendo las pruebas existentes de normalización de reasoning.

- [ ] **Step 5: Commitar el renderer de streaming**

```bash
git add crates/gpui/src/ui/markdown.rs
git commit -m "feat: animate the active streaming text line"
```

### Task 3: Integrar contenido nuevo y estados de UI

**Files:**
- Modify: `crates/gpui/src/app.rs`

- [ ] **Step 1: Importar las constantes y el helper**

Extender el `use crate::ui::{...}` existente para incluir `animation::{fade_in, ATTACHMENT_FADE_DURATION, MESSAGE_FADE_DURATION, STATE_FADE_DURATION}` y eliminar la constante local `STREAM_MESSAGE_FADE_DURATION` después de migrar su uso.

- [ ] **Step 2: Animar chips e imágenes de adjuntos con IDs estables**

En `render_composer`, envolver cada chip terminado con `fade_in(..., format!("composer-attachment-{index}"), ATTACHMENT_FADE_DURATION)`. En `render_image_attachments`, envolver el contenedor de cada imagen con la clave `message-{message_id}-image-{index}`. No se animará el contenido interno ni se modificará el tamaño final.

- [ ] **Step 3: Animar cambios de estado de checkpoints y tareas**

Para estados que no sean `InProgress`, envolver el icono en un `Div` con `fade_in` y una clave que incluya sesión, ID y estado (`checkpoint-state-...-{status:?}` / `task-state-...-{status:?}`). Mantener los giros existentes para `Loader`; no añadir un segundo loop a esos iconos.

- [ ] **Step 4: Añadir una entrada breve al icono del composer**

Mantener el pulso del botón detener. Para el estado no procesando, usar un icono `ArrowUp` como hijo con `with_animation` y una clave `composer-send-icon-{session.id}`; la animación será una única entrada de opacidad/escala muy leve al montar el botón. El contrato de click, disabled y tooltip permanecerá intacto.

- [ ] **Step 5: Pasar una identidad estable al renderer de streaming**

Actualizar `render_assistant_text_segment` para recibir `stream_animation_id: &str` y llamar a:

```rust
render_streaming_markdown(theme, text, stream_animation_id)
```

Usar IDs derivados de sesión, mensaje y segmento (`stream-message-{session_id}-{message_index}-{segment_index}`); no usar `text.len()` como clave.

- [ ] **Step 6: Migrar el fade existente del mensaje del asistente**

Conservar `message.animate_in` y su condición, pero usar `MESSAGE_FADE_DURATION` desde `ui::animation`. El elemento estable `stream-message-fade-{session_id}-{index}` seguirá garantizando una sola reproducción durante los renders batched.

- [ ] **Step 7: Ejecutar formateo, compilación y tests dirigidos**

Run: `cargo fmt --all -- --check`

Expected: PASS.

Run: `cargo test -p averroes-gpui ui::markdown::tests ui::animation::tests -- --nocapture`

Expected: PASS.

Run: `cargo check -p averroes-gpui`

Expected: PASS.

- [ ] **Step 8: Commitar la integración**

```bash
git add crates/gpui/src/app.rs
git commit -m "feat: add subtle motion to chat states and composer"
```

### Task 4: Verificación final

**Files:**
- Verify: `crates/gpui/src/ui/animation.rs`
- Verify: `crates/gpui/src/ui/markdown.rs`
- Verify: `crates/gpui/src/app.rs`

- [ ] **Step 1: Ejecutar todos los tests del workspace**

Run: `cargo test --workspace`

Expected: PASS.

- [ ] **Step 2: Revisar que no haya animaciones infinitas nuevas fuera de estados activos**

Run: `rg -n "with_animation|Animation::new|STREAM_LINE_FADE|MESSAGE_FADE" crates/gpui/src/ui crates/gpui/src/app.rs`

Expected: los únicos `repeat()` nuevos deben ser inexistentes; los existentes de stop/spinner permanecen ligados a estados activos.

- [ ] **Step 3: Revisar el diff y el estado del repositorio**

Run: `git diff HEAD~3 --check && git status --short`

Expected: sin errores de whitespace; solo cambios de animación y documentación/commits de esta tarea.

