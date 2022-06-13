# webgrep (wg)

[![Build status](https://github.com/JustinLovinger/webgrep/workflows/build/badge.svg)](https://github.com/JustinLovinger/webgrep/actions)

Recursively search the web.

## Usage

`wg [OPTIONS] <PATTERN> <URL>...`.

See `wg --help` for full options.

### Caching

`webgrep` maintains a page cache
under `$XDG_CACHE_HOME/webgrep`,
default `~/.cache/webgrep`.
This cache allows searching the same URLs
for different terms
without repeating web requests.
Currently,
this page cache is not automatically culled.
It is safe to delete part or all of this cache
at any time.

## Building

- Build with `nix build`.
- Enter a development shell with `nix develop`.
