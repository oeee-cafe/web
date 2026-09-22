# Third-party notices

oeee-cafe is distributed under the GNU Affero General Public License v3; see
[LICENSE](LICENSE). This file records the third-party work it is built on top
of, and the terms that come with each.

Package dependencies declared in `Cargo.toml`, `package.json` and their lock
files are not repeated here. This is about code and reference material that
entered the repository some other way.

## Drawpile

<https://drawpile.net> — GNU General Public License, version 3 or later.

`neo-cucumber/src/utils/canvasHistory.ts` is derived from Drawpile's
`src/drawdance/libengine/dpengine/canvas_history.c`, Copyright (C) 2022
askmeaboutloom. It follows that file's entry list, savepoint scheme,
optimistic fork, and the concurrency check that decides between applying a
remote message directly and replaying history beneath it.

AGPL-3 section 13 permits an AGPL-3 work to be combined with GPL-3 code, so
the combination is distributed under the AGPL-3 with this notice preserved.

Several other files name Drawpile in comments — the server's session history
and reset handling in `src/web/handlers/collaborate/`, and the pointer
encoding in `frontend/collaborate/binaryProtocol.ts`. Those describe a design
that was studied and then written independently; no Drawpile code was carried
into them.

## Material Symbols

<https://github.com/google/material-design-icons> — Apache License 2.0,
Copyright Google.

The painter draws its tools with Material Symbols through the
`@iconify-json/material-symbols` package. The profile's achievement icons in
`templates/achievement_icon_macro.jinja` are the same artwork -- `brush`,
`reply`, `group`, `sports-esports` and `star` -- with their path data copied
from that package into the template, since server-rendered pages cannot load
it. They are unmodified.

## PaintBBS NEO

<https://github.com/yangnaru/neo> — included as the git submodule `neo`.

The submodule is a reference and a test fixture, not a dependency: it is not
built into anything we ship, and it is not redistributed here — cloning it is
`git submodule update`, which fetches it from its own repository. Our tests
load `neo/src/painter.js` into the test page so that our rasterisation and our
`.pch` files are checked against the canonical implementation rather than
against a description of it. `neo-cucumber` reimplements NEO's behaviour,
quirks included, from those observed results.

**Its licence terms are not stated in that repository** — there is no licence
file and no header in the sources. This is unresolved rather than settled, and
worth clearing up with the upstream authors before anything from it is
redistributed or before its terms are relied on.

## tegaki

<https://github.com/desuwa/tegaki> — MIT, Copyright (c) 2015 Maxime Youdine.
Included as the git submodule `tegaki`.

Unlike `neo`, this one really is shipped: the Dockerfile copies it into the
image, and `tegaki/css`, `tegaki/js` and `tegaki/lib` are served under
`/static/tegaki/` to anyone opening a tegaki replay. MIT asks that its notice
travel with the copies and none of those files carry a header, so
`tegaki/LICENSE` is served alongside them at `/static/tegaki/LICENSE`.

`tegaki/lib/UZIP` is a vendored copy of UZIP.js with its own licence, which
sits beside it under the same served path.
