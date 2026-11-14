# License texts

The standard texts of the licenses on the allow-list (`deny.toml`), taken
unmodified from the SPDX License List data
(<https://github.com/spdx/license-list-data>, `text/<SPDX id>.txt`).
`scripts/third-party-licenses.py` renders them once per license into
`THIRDPARTY-NOTICES.md`.

Add a text when the allow-list gains a license:

```sh
curl -fsS https://raw.githubusercontent.com/spdx/license-list-data/main/text/<id>.txt -o scripts/license-texts/<id>.txt
```
