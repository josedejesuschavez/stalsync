# stalsync

Sincronización **N-way de discos locales** escrita en Rust. Sincroniza 2 o más
discos (carpetas o puntos de montaje) para que converjan al mismo contenido,
sin maestro: cualquier disco puede cambiar y los demás lo reciben.

```
~/hdd1 ─┐
~/hdd2 ─┼─ stalsync sync ~/hdd1 ~/hdd2 ~/hdd3
~/hdd3 ─┘
```

## Características

- **Peer-to-peer**: no hay disco maestro. Ediciones, altas y borrados en
  cualquier disco se propagan a los demás.
- **Detección eficiente (HASH_THEN_MTIME)**: si tamaño y mtime coinciden con el
  manifiesto, no se lee ni hashea el archivo. El re-sync típico cuesta recorrer
  + `stat`, no I/O de contenido.
- **Hashes BLAKE3** para identificar contenido: el hash es la verdad, el mtime
  es solo una optimización.
- **Resolución de conflictos**: si dos discos modifican el mismo archivo con
  contenido distinto, gana el `mtime` más reciente; la versión perdedora se
  conserva renombrada en su disco (`*.conflict-<disco>-<ts>`) y **nunca se
  propaga**.
- **Papelera de borrados**: los borrados propagados no hacen unlink() sino
  rename a `<disco>/.stalsync/trash/<timestamp>/<ruta>` (instantáneo, mismo
  filesystem). Un borrado accidental es reversible y el rescate repropaga
  solo. Retención configurable con `--trash-days` (0 = desactivada).
- **Política conservadora**: una modificación gana sobre un borrado (borrar
  puede ser un accidente; una edición no debe perderse).
- **Manifiesto por disco = caché, no verdad**: cada disco guarda su estado
  acordado en `<disco>/.stalsync/manifest.jsonl`. Si se pierde o corrompe, la
  siguiente pasada lo reconstruye — el contenido idéntico no se recopia.
- **Escritura atómica de manifiestos** (tmp + fsync + rename) y copia con
  `fsync` + **verificación BLAKE3 del destino**: un corte o un destino
  corrupto a mitad de sync no deja estado intermedio comprometido.
- **Permisos POSIX preservados**: el mode viaja en el manifiesto y se aplica
  al copiar; un `chmod` se propaga como un cambio más (chmod barato, sin
  recopiar contenido). `--no-perms` desactiva. En Windows no aplica (no hay
  modes POSIX; los manifiestos son intercambiables entre SO).
- **Sin dependencias en runtime**: un solo binario estático.

## Requisitos

- Rust (toolchain stable, edition 2024). Instalar con [rustup](https://rustup.rs/):
  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```
- Sistema de archivos con mtime de alta resolución y rename atómico
  (ext4, XFS, Btrfs, APFS…). **exFAT/FAT32 no está soportado** (mtime de 2s
  rompe la detección de cambios). **NTFS (Windows) funciona**, pero redondea
  mtimes a intervalos de 100ns: precisión mayor se pierde al escribir y
  stalsync lo tolera vía hash (el contenido manda, no el mtime).

## Instalación

```bash
git clone <repo> && cd stalsync/stalsync
cargo build --release
```

El binario queda en `target/release/stalsync`. Para instalarlo en el PATH:

```bash
cargo install --path .
```

## Uso

```bash
# Sincronizar N discos (mínimo 2)
stalsync sync ~/hdd1 ~/hdd2 ~/hdd3

# Ver el plan sin ejecutar nada
stalsync sync ~/hdd1 ~/hdd2 ~/hdd3 --dry-run

# Detalle por archivo (log scrolleable)
stalsync sync ~/hdd1 ~/hdd2 ~/hdd3 -v

# Retención de papelera distinta (default 30 días; 0 = desactivada)
stalsync sync ~/hdd1 ~/hdd2 ~/hdd3 --trash-days 7

# No preservar permisos POSIX al copiar (por default SÍ se preservan)
stalsync sync ~/hdd1 ~/hdd2 ~/hdd3 --no-perms

# Diagnóstico de desfase (solo lectura, no toca nada)
stalsync status ~/hdd1 ~/hdd2 ~/hdd3

# Reconstruir/actualizar manifiestos desde el contenido real
stalsync refresh ~/hdd1 ~/hdd2 ~/hdd3 --yes
```

### `status`: ¿están al día los discos?

Ejecuta el mismo análisis que un sync (scan + merge) pero **sin modificar
nada**. Reporta por disco los cambios locales sin propagar, lo que recibirá,
environará o perderá, y los conflictos. Exit code scriptable: `0` = convergido,
`1` = hay desfase (usable en cron o scripts de monitoreo).

```
✔ todo sincronizado — 3 discos, 31202 rutas gestionadas, nada pendiente
```

```
⚠ 2 disco(s) desfasado(s) — 4 transferencias pendientes (340.0 MiB), 31202 rutas gestionadas

disco      locales (altas/bajas)   envía   recibe              perderá   conflictos
hdd1       2/0 (340.0 MiB)             2       0                   0         0  ⚠
hdd2       0                           0       2 (340.0 MiB)     0         0  ⚠
hdd3       0                           0       0                   0         0  ✔
```

La salida de progreso se adapta al contexto, como rsync/rclone/cargo:

- **Terminal interactiva**: spinner de escaneo y **una barra por disco
  destino** con sus bytes, velocidad y ETA; el plan incluye el desglose
  `por disco: hdd2: 30.0 GiB · hdd3: 30.0 GiB`.
- **Pipe o CI (sin TTY)**: silenciosa durante la pasada; solo el plan y el
  resumen final, aptos para logs.
- **`-v`/`--verbose`**: una línea por archivo, independientemente del TTY.

```
plan: 138 copias (54.0 MiB), 0 borrados, 0 conflictos, 69 rutas en estado acordado
  por disco: /home/jchavez/hdd2: 27.0 MiB · /home/jchavez/hdd3: 27.0 MiB
✔ completo en 0.9s — 138 archivos copiados (54.0 MiB), 0 borrados, 0 conflictos
```

Los conflictos se anuncian siempre al final (aunque no haya TTY), con la lista
de versiones perdedoras conservadas.

### Papelera: cómo rescatar un borrado

Cuando un borrado se propaga, el archivo no desaparece: se mueve a la
papelera del disco que lo perdió:

```
~/hdd2/.stalsync/trash/20261101_224733_123456789/docs/contrato.pdf
```

Para rescatarlo, copia o mueve el archivo de vuelta a su ruta original en
cualquiera de los discos; el siguiente `sync` lo repropaga a los demás. La
papelera se purga sola al inicio de cada sync (entradas con más de
`--trash-days` días) y nunca interfiere con la sincronización (vive bajo
`.stalsync/`, excluida del escaneo).

### `refresh`: reescribir manifiestos desde la realidad actual

Re-escanea cada disco y reescribe su manifiesto (`.stalsync/manifest.jsonl`)
para que refleje el contenido actual, **sin copiar ni borrar archivos**.
Sirve para reconstruir manifiestos perdidos, materializar hashes tras copias
manuales con `cp`, o re-baselinear tras resolver conflictos a mano.

> ⚠ **Semántica**: refresh hace que los cambios pendientes de propagar se
> olviden. Un borrado pendiente hará que el archivo vuelva a copiarse desde
> los discos que aún lo tienen; una modificación pendiente puede revertirse
> por la regla de mayoría. Por eso pide confirmación (o `--yes`).

```
✔ manifiestos actualizados a la realidad actual:
  hdd1       31202 entradas (31000 intactas, 202 actualizadas, 0 eliminadas del manifiesto)
```

El flujo de trabajo habitual: editas/borras archivos en cualquiera de los
discos y vuelves a ejecutar el comando. Los re-syncs sin cambios no copian
nada.

### ¿Qué sincroniza y qué no?

| Sincroniza | No sincroniza |
|---|---|
| Archivos regulares, con mtime preservado | `.stalsync/` (manifiestos) |
| Altas, ediciones y borrados desde cualquier disco | `*.conflict-*` (versiones perdedoras de conflictos) |
| Propagación de borrados (con política anti-pérdida) | Symlinks, hardlinks, xattrs (fase futura) |

## Configuración

stalsync no requiere archivo de configuración: los discos se pasan como
argumentos y el estado vive en cada disco.

- **Manifiesto**: `<disco>/.stalsync/manifest.jsonl` (JSON Lines: primera
  línea `{"format":N}` con la versión del formato — un lector viejo la
  ignora con warning — y entradas `path`, `size`, `mtime_ns`, `hash`). Es
  caché interna; no debe editarse a mano, pero borrarlo es seguro — se
  reconstruye en el siguiente sync. Un manifiesto de una versión *nueva* de
  stalsync se rechaza con error claro, no se interpreta como basura.
- **Archivos de conflicto**: `<nombre>.conflict-<disco>-<mtime_ns>`, solo en el
  disco perdedor. Su resolución es manual: el usuario decide cuál versión
  queda y borra el resto.
- **Política ante conflictos** (actual): gana el `mtime` más reciente; empates
  y desactualización de manifiestos se resuelven por mayoría de contenido.

## Cómo funciona

Cada pasada ejecuta cuatro fases:

1. **Scan**: cada disco se compara contra *su propio* manifiesto. Un archivo
   cuyo `size`+`mtime` coinciden se da por sin cambios; si difieren, se
   calcula BLAKE3 para confirmar.
2. **Merge N-way**: los deltas de cada disco se combinan en un estado acordado
   (una entrada por ruta). Solo las entradas *modificadas* compiten como
   versiones nuevas; un disco desactualizado no es un conflicto.
3. **Ejecución**: el origen se lee una vez y se escribe a N destinos en el
   mismo bucle (búfer de 256 KiB), con `fsync` y verificación BLAKE3 del
   destino contra el hash acordado (tamaño + contenido).
4. **Commit**: si todo salió bien, se reescriben los manifiestos de todos los
   discos de forma atómica. Si algo falló, ninguno se toca y la próxima pasada
   se autocura.

El diseño completo —cada regla y su justificación— no vive en un documento,
sino en los tests: cada uno protege una decisión y explica el *porqué* en su
comentario (`REGLA:`, `PROMESA:`, `INVARIANTE:`).

## Tests

```bash
cargo test                # toda la suite
cargo test --test merge_states   # batería 1: reglas del merge
cargo test --test sync_e2e       # batería 2: comportamiento end-to-end
cargo test --test resilience     # batería 3: invariantes y recuperación
```

| Batería | Cubre |
|---|---|
| `status` (6) | Comando status: convergencia, disco atrasado, cambios/borrados locales, conflictos, que no toca nada |
| `merge_states` (10) | Tabla de estados del merge: propagación, borrados, conflictos, resucitación, regresión Clean-vs-Changed |
| `sync_e2e` (8) | Primer sync, idempotencia, propagación desde cualquier disco, mtime-only sin copia, conflicto end-to-end, disco nuevo |
| `resilience` (7) | Manifiesto borrado/corrupto, atomicidad, disco desconectado, convergencia desde caos, estabilidad tras conflicto |

Los tests usan discos temporales (`tempfile`) y no tocan datos reales.

## Estructura

```
stalsync/
├── src/
│   ├── main.rs      # CLI (thin wrapper)
│   ├── lib.rs
│   ├── scan.rs      # recorrido + HASH_THEN_MTIME + BLAKE3
│   ├── merge.rs     # merge N-way y plan de acciones
│   ├── sync.rs      # ejecución: copia leer-una-vez-escribir-N
│   └── manifest.rs  # manifiesto JSONL atómico
└── tests/
    ├── common/mod.rs     # helpers: discos temporales, tree_hash
    ├── merge_states.rs
    ├── sync_e2e.rs
    └── resilience.rs
```

## Limitaciones conocidas y roadmap

- **Renames**: un archivo movido se trata como borrado + nuevo (recopia).
  Detección por `(size, hash)` planificada.
- **Escaneo y hashing secuenciales**: paralelización con `rayon` para árboles
  grandes.
- **Symlinks, hardlinks, xattrs, sparse files**: no gestionados (se ignoran).
- **`io_uring`** para árboles con muchos archivos pequeños.
- **exFAT/FAT32**: fuera de soporte por mtime de 2s.
