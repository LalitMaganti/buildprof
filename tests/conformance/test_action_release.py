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
    (tmp_path / "README.md").write_text(dedent(f'''\
        steps:
          - uses: LalitMaganti/buildprof@{release["PREVIEW_REF"]} {release["PREVIEW_NOTE"]}
    '''))
    return tmp_path


@pytest.mark.parametrize("version", ["0.2.6", "0.3.0-rc.1"])
def test_prepare_next_release(release_tree: Path, version: str) -> None:
    cargo = release_tree / "Cargo.toml"
    cargo.write_text(cargo.read_text().replace('version = "0.2.5"', f'version = "{version}"', 1))
    before = {path: path.read_text() for path in release_tree.iterdir()}

    assert set(prepare(release_tree, check=True, allow_preview=True)) == {
        "action.yml", "README.md"
    }
    assert all(path.read_text() == text for path, text in before.items())
    prepare(release_tree, check=False, allow_preview=False)

    assert f"    default: v{version}\n" in (release_tree / "action.yml").read_text()
    readme = (release_tree / "README.md").read_text()
    assert f"uses: LalitMaganti/buildprof@v{version}\n" in readme
    assert release["PREVIEW_NOTE"] not in readme
    assert prepare(release_tree, check=True, allow_preview=False) == []
    assert prepare(release_tree, check=False, allow_preview=False) == []


def test_preview_is_not_releasable(release_tree: Path) -> None:
    assert prepare(release_tree, check=True, allow_preview=True) == []
    assert prepare(release_tree, check=True, allow_preview=False) == ["README.md"]


@pytest.mark.parametrize(
    ("name", "original", "replacement"),
    [
        ("action.yml", "default: v0.2.5", "default: v0.1.0"),
        ("README.md", "@v0.2.5", "@v0.1.0"),
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
    readme = release_tree / "README.md"
    readme.write_text("Missing the action example.\n")
    action = release_tree / "action.yml"
    action.write_text(action.read_text().replace("default: v0.2.5", "default: v0.1.0"))
    before = action.read_text()

    with pytest.raises(ValueError, match="expected one match"):
        prepare(release_tree, check=False, allow_preview=False)

    assert action.read_text() == before
