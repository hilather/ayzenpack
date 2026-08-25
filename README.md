# ayzenpack

Dehydrate many JARs into one zstd content-addressed archive and rehydrate them.

Archive files use magic `AYZP` and the recommended extension `.ayz`.

## Commands

```text
ayzenpack dehydrate -o libs.ayz app.jar lib/*.jar
ayzenpack rehydrate -i libs.ayz -d restored/
```

Aliases: `pack` = `dehydrate`, `unpack` = `rehydrate`.

These commands are not implemented yet.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
