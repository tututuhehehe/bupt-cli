"""Offline tests for the headless-Chrome profile lifecycle.

A Chrome profile is 6-150 MB, so a leaked one dwarfs every other cache the CLI
writes. Cleanup has to survive Ctrl-C (atexit) and, for SIGKILL, be repaired by
the next run.
"""

import os
import time

from bupt_cli import grad_browser


def _make_profile(tmp_path, name: str, age_seconds: float):
    profile = tmp_path / name
    profile.mkdir()
    (profile / "blob.bin").write_bytes(b"x" * 64)
    stamp = time.time() - age_seconds
    os.utime(profile, (stamp, stamp))
    return profile


def test_sweep_removes_only_stale_bupt_profiles(tmp_path, monkeypatch):
    monkeypatch.setattr(grad_browser.tempfile, "gettempdir", lambda: str(tmp_path))

    stale = _make_profile(
        tmp_path, "bupt-grad-stale", grad_browser.STALE_PROFILE_SECONDS + 60
    )
    fresh = _make_profile(tmp_path, "bupt-grad-fresh", 60)
    unrelated = _make_profile(tmp_path, "some-other-app", 10 * 24 * 3600)

    removed = grad_browser.sweep_stale_profiles()

    assert removed == 1
    assert not stale.exists()
    # A recent profile could belong to a concurrent run.
    assert fresh.exists()
    assert unrelated.exists()


def test_sweep_ignores_symlinks(tmp_path, monkeypatch):
    """A symlink must not be followed into someone else's directory."""
    monkeypatch.setattr(grad_browser.tempfile, "gettempdir", lambda: str(tmp_path))

    target = tmp_path / "real-data"
    target.mkdir()
    (target / "keep.txt").write_text("important", encoding="utf-8")

    link = tmp_path / "bupt-grad-link"
    link.symlink_to(target, target_is_directory=True)
    old = time.time() - grad_browser.STALE_PROFILE_SECONDS - 60
    os.utime(link, (old, old), follow_symlinks=False)

    grad_browser.sweep_stale_profiles()

    assert target.exists()
    assert (target / "keep.txt").exists()


def test_sweep_returns_zero_on_an_empty_temp_dir(tmp_path, monkeypatch):
    monkeypatch.setattr(grad_browser.tempfile, "gettempdir", lambda: str(tmp_path))
    assert grad_browser.sweep_stale_profiles() == 0


def test_close_removes_the_profile(tmp_path, monkeypatch):
    """`close()` must delete the profile even if called twice."""
    monkeypatch.setattr(grad_browser.tempfile, "gettempdir", lambda: str(tmp_path))
    profile = tmp_path / "bupt-grad-close"
    profile.mkdir()

    driver = grad_browser.Driver.__new__(grad_browser.Driver)
    driver.profile = profile
    driver.port = 0

    driver._remove_profile()
    assert not profile.exists()
    driver._remove_profile()  # idempotent
