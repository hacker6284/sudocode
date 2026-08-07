#!/usr/bin/env python3
"""Inject a release's freshly-computed binary sha256s into rules_sudo/versions.bzl.

The matched-pair release (design §8 Phase 5 Task 8, Fable's Phase 4.5 guidance)
has a chicken-and-egg: the rules_sudo tarball published INSIDE release <tag> must
pin that release's own binary sha256s, but those only exist after the binaries
build. `release.yml` therefore builds all four assets `-c opt` per platform,
computes each `<asset>-<triple>.sha256`, and runs this script to REGENERATE the
`_PENDING` block in versions.bzl (between the `# INJECT-RELEASE-BEGIN/END`
markers) BEFORE packaging the tarball. The tag is a required CLI argument — the
in-repo pending block just names the next planned tag with empty shas.

Usage:
    inject_release_shas.py <versions.bzl> <sha_dir> <tag>

<tag> must match ^v[0-9]+\\.[0-9]+\\.[0-9]+$ (full-string). <sha_dir> holds one
`<asset>-<triple>.sha256` file per built binary (shasum -a 256 format:
"<hex>  <name>"). Every (asset, platform) the release publishes must be present,
or this fails loudly — a partial injection would ship a half-pinned manifest.
The marker BEGIN/END pair is the only contract with versions.bzl; the
platform/asset sets mirror it (kept in sync by //tools:inject_release_shas_test).
"""
import re
import sys

# Mirror versions.bzl PLATFORM_TRIPLES / RELEASE_ASSETS. Order here is the
# canonical emission order for the regenerated block.
VAR_NAME = "_PENDING"
VERSION_VAR = "_PENDING_VERSION"
BEGIN = "# INJECT-RELEASE-BEGIN"
END = "# INJECT-RELEASE-END"
TAG_RE = re.compile(r"^v[0-9]+\.[0-9]+\.[0-9]+$")
PLATFORM_TRIPLES = [
    ("macos_arm64", "aarch64-apple-darwin"),
    ("linux_x86_64", "x86_64-unknown-linux-gnu"),
    ("linux_aarch64", "aarch64-unknown-linux-gnu"),
]
RELEASE_ASSETS = ["sudoc", "lockstep_diff", "capture_run", "emit_unpack"]


def read_sha(sha_dir, asset, triple):
    path = "%s/%s-%s.sha256" % (sha_dir, asset, triple)
    try:
        with open(path) as fh:
            first = fh.read().split()
    except FileNotFoundError:
        sys.exit("inject_release_shas: missing sha file %s (asset %s / %s). "
                 "The release must build every asset on every platform."
                 % (path, asset, triple))
    if not first:
        sys.exit("inject_release_shas: empty sha file %s" % path)
    sha = first[0].strip().lower()
    if len(sha) != 64 or any(c not in "0123456789abcdef" for c in sha):
        sys.exit("inject_release_shas: %s does not hold a 64-hex sha256 (got %r)"
                 % (path, sha))
    return sha


def render_block(sha_dir, tag):
    lines = [
        BEGIN,
        '%s = "%s"' % (VERSION_VAR, tag),
        "%s = {" % VAR_NAME,
    ]
    for asset in RELEASE_ASSETS:
        lines.append('    "%s": {' % asset)
        for platform_key, triple in PLATFORM_TRIPLES:
            sha = read_sha(sha_dir, asset, triple)
            lines.append('        "%s": "%s",' % (platform_key, sha))
        lines.append("    },")
    lines.append("}")
    lines.append(END)
    return "\n".join(lines)


def main(argv):
    if len(argv) != 4:
        sys.exit("usage: inject_release_shas.py <versions.bzl> <sha_dir> <tag>")
    versions_path, sha_dir, tag = argv[1], argv[2], argv[3]
    if not TAG_RE.match(tag):
        sys.exit("inject_release_shas: tag %r does not match ^v[0-9]+\\.[0-9]+\\.[0-9]+$"
                 % tag)
    with open(versions_path) as fh:
        src = fh.read()
    if BEGIN not in src or END not in src:
        sys.exit("inject_release_shas: markers %s / %s not found in %s"
                 % (BEGIN, END, versions_path))
    head, rest = src.split(BEGIN, 1)
    _, tail = rest.split(END, 1)
    new_src = head + render_block(sha_dir, tag) + tail
    with open(versions_path, "w") as fh:
        fh.write(new_src)
    print("inject_release_shas: injected %d assets x %d platforms as %s into %s"
          % (len(RELEASE_ASSETS), len(PLATFORM_TRIPLES), tag, versions_path))


if __name__ == "__main__":
    main(sys.argv)
