# stalsync — Diseño

Sincronización N-way de discos locales (3 discos: `~/hdd1`, `~/hdd2`, `~/hdd3`).
Modelo peer-to-peer: ningún disco es maestro; al final de cada pasada exitosa
los N discos convergen al mismo estado de contenido.

## Decisiones fijadas

### Filesystem objetivo
- **ext4** (las 3 unidades): mtime con resolución de nanosegundos, rename atómico,
  symlinks y xattrs disponibles.
- exFAT/FAT32 queda fuera de soporte inicial (mtime de 2s rompe HASH_THEN_MTIME).

### Manifiesto
- Ubicación: `<disco>/.stalsync/manifest.jsonl` (JSON Lines, una entrada por archivo).
- El manifiesto es **caché, no verdad**: si se pierde o desincroniza, la siguiente
  pasada rehashea y se autocura. La verdad es el contenido (hash) en disco.
- Contenido por entrada: `path` (relativo al root del disco), `size`, `mtime_ns`,
  `hash` (BLAKE3 en hex).
- Escritura atómica: `manifest.jsonl.tmp` → `fsync` → `rename`.
- Se reescribe **solo al final** de una pasada sin errores. Si la pasada falla a
  mitad, no se toca ningún manifiesto; la próxima pasada se autocura
  (los hashes que ya coinciden no se recopian).

### Detección de cambios (HASH_THEN_MTIME)
- Si `size` y `mtime_ns` coinciden con el manifiesto → sin cambios, **no se hashea**.
- Si difieren → se calcula BLAKE3: hash igual = solo cambió mtime (se actualiza
  el manifiesto); hash distinto = modificado.
- Archivo en manifiesto pero no en disco → borrado.

### Estados por disco y ruta
`Clean` (igual a su manifiesto), `Changed` (contenido nuevo/distinto),
`Deleted` (estaba en manifiesto, ya no existe), `Absent` (nunca estuvo).

### Merge N-way (reglas)
- 0 cambios → nada.
- 1+ discos con cambios, todos con el **mismo hash** → propagar a los demás
  (incluidos los que borraron: la modificación **resucita** el archivo).
- Ningún cambio, pero hay borrados → propagar el borrado a los que aún lo tienen.
- Hashes **distintos** entre discos → conflicto: gana el `mtime_ns` más reciente;
  los perdedores se renombran a `path.conflict-<disco>-<mtime_ns>` y reciben
  el contenido ganador.

### Archivos de conflicto
- Vive **solo** en el disco perdedor, con sufijo `.conflict-<disco>-<ts>`.
- **No entran al manifiesto ni se sincronizan** (patrón ignorado en el escaneo).
  Así no se propagan ni se multiplican. Resolución manual del usuario.

### Exclusiones de escaneo
- Directorio `.stalsync/` (manifiestos).
- Nombres que contienen `.conflict-`.

### Copia
- **Leer una vez, escribir a N destinos** en el mismo bucle (búfer de 256 KiB).
- Se preserva `mtime` en los destinos (`filetime`).
- `fsync` por archivo copiado (durabilidad antes de dar el sync por bueno).
- Verificación: tamaño del destino == tamaño esperado.

### CLI
```
stalsync sync <disco1> <disco2> [discoN...] [--dry-run]
```

## Fases futuras (no implementado aún)
- Detección de rename/move por (size, hash) para evitar recopias.
- Paralelización del escaneo y hashing (rayon).
- io_uring para árboles con muchos archivos pequeños.
- Verificación post-copia por hash completo.
- Borrado de directorios vacíos tras eliminar archivos.
- Sparse files, symlinks, xattrs (política a definir).
