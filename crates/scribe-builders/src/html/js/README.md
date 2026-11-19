# Vendored Snowball stemmer JavaScript (Phase 18)

`base-stemmer.js` and `english-stemmer.js` are byte-for-byte copies of the
**minified** Snowball stemmer assets shipped with Sphinx 9.3.0 at
`sphinx/search/minified-js/`:

- `sphinx/sphinx/search/minified-js/base-stemmer.js`
- `sphinx/sphinx/search/minified-js/english-stemmer.js`

These files are generated from the upstream Snowball project
(<https://github.com/snowballstem/snowball-website>) and distributed by Sphinx
under its BSD-3-Clause license; the Snowball algorithms themselves are in the
public domain. They are embedded verbatim into `language_data.js` (as upstream
does) so the vendored `searchtools.js` defines `window.Stemmer` and stems
client-side queries with the same English algorithm as `rust-stemmers` on the
server side.

They are also copied into `_static/` for parity with upstream's
`copy_stemmer_js()`. Do not hand-edit.
