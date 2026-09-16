# Copyright 2026 The Buildprof Authors.
# SPDX-License-Identifier: Apache-2.0

from __future__ import annotations

from pathlib import Path
import runpy
from textwrap import dedent

import pytest


ROOT = Path(__file__).resolve().parents[2]
release = runpy.run_path(str(ROOT / "infra/prepare-action-release"))
prepare = release["prepare"]


@pytest.fixture
def release_tree(tmp_path: Path) -> Path:
    (tmp_path / "Cargo.toml").write_text(dedent('''\
        [package]
        name = "buildprof"
        version = "0.2.5"
    '''))
    (tmp_path / "action.yml").write_text(dedent('''\
        inputs:
          version:
            description: Buildprof release tag to install.
            default: v0.2.5
    '''))
    (tmp_path / "docs").mkdir()
    (tmp_path / "docs/usage.md").write_text(dedent(f'''\
        steps:
          - uses: LalitMaganti/buildprof@{release["PREVIEW_REF"]} {release["PREVIEW_NOTE"]}
    '''))
    (tmp_path / "CHANGELOG.md").write_text(dedent('''\
        ## [Unreleased]

        ## [0.2.5] - 2026-09-11

        ## [0.2.4] - 2026-09-10
    '''))
    # The test that installs a release names the one before this version.
    (tmp_path / ".github/workflows").mkdir(parents=True)
    # Indented as the workflow is, since that is what the pattern anchors on.
    (tmp_path / ".github/workflows/ci.yml").write_text(
        "      - name: Record with an installed release\n"
        "        uses: ./\n"
        "        with:\n"
        "          version: v0.2.4\n"
    )
    return tmp_path


@pytest.mark.parametrize("version", ["0.2.6", "0.3.0-rc.1"])
def test_prepare_next_release(release_tree: Path, version: str) -> None:
    cargo = release_tree / "Cargo.toml"
    cargo.write_text(cargo.read_text().replace('version = "0.2.5"', f'version = "{version}"', 1))
    before = {
        path: path.read_text() for path in release_tree.rglob("*") if path.is_file()
    }

    assert set(prepare(release_tree, check=True, allow_preview=True)) == {
        "action.yml", "docs/usage.md", ".github/workflows/ci.yml"
    }
    assert all(path.read_text() == text for path, text in before.items())
    prepare(release_tree, check=False, allow_preview=False)

    assert f"    default: v{version}\n" in (release_tree / "action.yml").read_text()
    usage = (release_tree / "docs/usage.md").read_text()
    assert f"uses: LalitMaganti/buildprof@v{version}\n" in usage
    assert release["PREVIEW_NOTE"] not in usage
    workflow = (release_tree / ".github/workflows/ci.yml").read_text()
    # The release under test has no assets, so CI installs the one before it.
    assert "          version: v0.2.5\n" in workflow
    assert prepare(release_tree, check=True, allow_preview=False) == []
    assert prepare(release_tree, check=False, allow_preview=False) == []


def test_preview_is_not_releasable(release_tree: Path) -> None:
    assert prepare(release_tree, check=True, allow_preview=True) == []
    assert prepare(release_tree, check=True, allow_preview=False) == ["docs/usage.md"]


@pytest.mark.parametrize(
    ("name", "original", "replacement"),
    [
        ("action.yml", "default: v0.2.5", "default: v0.1.0"),
        ("docs/usage.md", "@v0.2.5", "@v0.1.0"),
        (".github/workflows/ci.yml", "version: v0.2.4", "version: v0.1.0"),
    ],
)
def test_check_detects_drift(
    release_tree: Path, name: str, original: str, replacement: str
) -> None:
    prepare(release_tree, check=False, allow_preview=False)
    path = release_tree / name
    path.write_text(path.read_text().replace(original, replacement))

    assert prepare(release_tree, check=True, allow_preview=False) == [name]


def test_invalid_documentation_does_not_partially_update(release_tree: Path) -> None:
    usage = release_tree / "docs/usage.md"
    usage.write_text("Missing the action example.\n")
    action = release_tree / "action.yml"
    action.write_text(action.read_text().replace("default: v0.2.5", "default: v0.1.0"))
    before = action.read_text()

    with pytest.raises(ValueError, match="expected one match"):
        prepare(release_tree, check=False, allow_preview=False)

    assert action.read_text() == before
