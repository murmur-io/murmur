# Contributing to Murmur

Thanks for your interest. Murmur is licensed under **GNU AGPL-3.0** (see [`LICENSE`](LICENSE)).

## Why a CLA

Murmur asks every contributor to sign a **Contributor License Agreement (CLA)** — see [`CLA.md`](CLA.md). This is **not** to take your rights away (you keep full ownership of your contribution and can use it however you like). It exists so the project maintainer holds a clear, consolidated **relicensing grant** over the whole codebase.

That grant is what makes Murmur's **dual-licensing model** possible: the code stays free and open under AGPL-3.0 for everyone, while the maintainer retains the ability to offer a **commercial exception** to organizations that cannot use AGPL (e.g. those embedding Murmur's lock / redaction / facts code into a proprietary or self-hosted network service). Under a pure-AGPL project with scattered copyright, *no one* can grant that exception — every contributor would have to individually agree. The CLA is the standard mechanism used by AGPL projects like **Grafana** and **GitLab**, and dual-licensed projects like **MySQL**, for exactly this reason. It **strengthens** the project's independence; it does not weaken your rights.

## How to contribute

1. Open an issue or discuss the change first for anything non-trivial.
2. Branch from `murmur`, keep the change focused, and follow the repo conventions (`CLAUDE.md`, `.claude/rules/`).
3. Make sure `bash scripts/ci.sh` is green (clippy `-D warnings` + `cargo test --lib` + `ng lint` + `ng build` + E2E).
4. Open a PR against `murmur`. The CLA check must pass before merge.
5. Sign the CLA — see [`CLA.md`](CLA.md). (Once the CLA bot is wired, a PR comment will prompt you to sign once; until then, add a `Signed-off-by: Name <email>` DCO line to your commits and note your CLA agreement in the PR.)

## Status of the CLA (maintainer to finalize)

> ⚠️ **The CLA in [`CLA.md`](CLA.md) is a DRAFT.** Before it is binding the maintainer must decide: (1) the **rights-holding entity** (individual vs company), (2) **relicensing grant vs full assignment** (a grant is recommended), and (3) collect **retroactive sign-off** from existing contributors who have not signed. Prior contributors with unsigned commits will be contacted for a retroactive sign-off (or a clean-room replacement of those contributions) before any commercial license is offered over the affected code.
