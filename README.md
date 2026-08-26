# ayzenpack

Dehydrate many JARs into one zstd content-addressed archive and rehydrate them.

Archive files use magic `AYZP` and the recommended extension `.ayz`.

## Commands

```text
ayzenpack dehydrate -o libs.ayz app.jar lib/*.jar
ayzenpack rehydrate -i libs.ayz -d restored/
```

Aliases: `pack` = `dehydrate`, `unpack` = `rehydrate`. Also `list` and `verify`.

`dehydrate` (alias `pack`) writes a `.ayz` archive and overwrites `-o` if it exists.

Progress (per JAR, by entry count) and the final stats line go to **stderr**. `-q` / `--quiet` disables progress. stdout stays empty on success so the binary is pipe-safe.

```text
ayzenpack: 12 jars, 8401 entries, 912 unique blobs, 148.2 MiB → 41.7 MiB unique, zstd 18.4 MiB (0.124 of jar bytes)
```

`--json-logs` writes one JSON object per event on stderr (not stdout).

The archive MANIFEST is compact JSON with `"format": "ayzenpack-manifest"`. See `schemas/manifest.v1.schema.json` and `examples/tiny.manifest.json`.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
