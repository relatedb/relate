# Third-party notices

Relate.Net is licensed under the Apache License, Version 2.0 (see
[`LICENSE`](LICENSE)). It adapts code contributed under the MIT License and
redistributes, unmodified, the native engine library; their notices follow.

## Upstream client code

- Component: the contributed Relate.Net client implementation
  (see [UPSTREAM.md](UPSTREAM.md))
- Copyright (c) 2026 Peter Villadsen
- License: MIT (full text below). The original notice is retained for the
  adapted code; the client as a whole is distributed under Apache-2.0
  ([ADR-0625](https://github.com/relatedb/relate/blob/main/docs/adr/0625-adopt-the-dotnet-client.md)).

```text
MIT License

Copyright (c) 2026 Peter Villadsen

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Relate native library

- Component: Relate C FFI library, built from the same repository revision as
  this client
- Source: https://github.com/relatedb/relate
- Native asset: `relate_ffi.dll`, `librelate_ffi.dylib` or `librelate_ffi.so`,
  according to the package RID
- Copyright 2026 The Relate Authors
- License: Apache License, Version 2.0 (full text in [`LICENSE`](LICENSE))
