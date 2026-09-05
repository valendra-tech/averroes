# Diseño: animaciones sutiles de interfaz

## Objetivo

Añadir una capa coherente de microanimaciones elegantes a las zonas más
visibles de la interfaz GPUI, manteniendo la sensación de rapidez y evitando
que el movimiento distraiga o provoque saltos de layout.

## Alcance aprobado

La primera iteración será selectiva y cubrirá:

- botones e iconos interactivos;
- composer, incluyendo adjuntos y transición entre enviar/detener;
- mensajes nuevos del asistente;
- texto mientras llega por streaming;
- cambios de estado de checkpoints y tareas.

No se animarán de forma global todos los elementos de la aplicación.

## Dirección técnica

Se reutilizará la API `with_animation` de GPUI y se crearán helpers pequeños
en `crates/gpui/src/ui/` para centralizar duraciones, easing y patrones
repetidos. Las claves de los elementos serán estables para que el render por
lotes no reinicie una animación en cada render.

La capa compartida contemplará estos patrones:

- `fade_in`: opacidad de 0 a 1 con desplazamiento vertical mínimo;
- `hover_lift`: realce y elevación de 1 px para controles interactivos;
- `state_pulse`: pulso discreto para estados activos;
- entrada y salida suave para contenido efímero, como adjuntos.

Las duraciones iniciales estarán en el rango de 140–220 ms. El streaming usará
una entrada más corta, de aproximadamente 120–160 ms.

## Streaming de texto

El flujo existente conserva una ventana de actualización de aproximadamente
32 ms. Esta ventana se mantiene para proteger la fluidez y evitar re-medidas
excesivas.

El mensaje del asistente hará un fade-in una sola vez. Durante el streaming,
solo el tramo final visible recibirá la entrada sutil; el contenido anterior
permanecerá estable. No se aplicará una animación completa al mensaje ni una
animación independiente por token.

Cuando el stream termine, el contenido pasará al render Markdown normal usando
una identidad estable y sin desplazamiento visible.

## Aplicaciones concretas

- **Botones:** transición de fondo y color, más una elevación de 1 px al hover.
- **Iconos:** pequeño desplazamiento o escala al hover; los giros se reservarán
  para loaders y spinners.
- **Adjuntos:** fade-in y desplazamiento lateral mínimo al añadirse; salida
  suave al eliminarse.
- **Checkpoints y tareas:** transición de color/ícono; los estados en progreso
  conservarán su pulso o giro actual.
- **Composer:** transición limpia entre enviar y detener, incluyendo el estado
  de procesamiento.
- **Mensajes:** aparición con fade-in y una subida leve.

## Estados y límites

Las animaciones infinitas solo se ejecutarán mientras exista un estado activo.
Las transiciones no deben modificar las dimensiones finales del contenido ni
crear saltos artificiales en la lista virtualizada.

## Verificación

Se ejecutará la compilación de `averroes-gpui` y la suite de tests existente.
Además se comprobará que:

1. las animaciones no se reinician con cada delta del stream;
2. las claves de los elementos permanecen estables;
3. el layout no cambia de tamaño por efectos de entrada o hover;
4. botones, iconos, composer, adjuntos, estados y streaming mantienen el
   aspecto y comportamiento esperados.

