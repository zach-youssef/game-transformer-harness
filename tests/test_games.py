"""games/testcore/game.toml through RetroEnv.from_game: ROM lookup, the reset script, RAM
variables, the score reward and the episode end (docs/games.md, docs/roms.md)."""

import hashlib
import tomllib
from collections.abc import Callable, Iterator
from pathlib import Path

import pytest

from gtharness import _native
from gtharness.env import GAMES_DIR, RetroEnv, core_library
from gtharness.errors import ConfigError, CoreLoadError
from gtharness.rewards import score_delta

TESTCORE_TOML = GAMES_DIR / "testcore" / "game.toml"
RETRO_MEMORY_SYSTEM_RAM = 2
START, A = 1 << 3, 1 << 8
RIGHT = 3


@pytest.fixture
def testcore(testcore_data: Path, testcore_path: Path) -> Iterator[Callable[..., RetroEnv]]:
    """Makes the test core game's env (closed after the test); keyword arguments override."""
    envs: list[RetroEnv] = []

    def make(**kwargs) -> RetroEnv:
        options = {
            "frameskip": 2,
            "obs_size": (64, 64),
            "action_format": "flat",
            "reward": score_delta,
            "core_path": testcore_path,
        } | kwargs
        envs.append(RetroEnv.from_game("testcore", testcore_data, **options))
        return envs[-1]

    yield make
    for env in envs:
        env.close()


def bcd(data: bytes) -> int:
    return int(data.hex())


def test_testcore_config_uses_every_field():
    raw = tomllib.loads(TESTCORE_TOML.read_text(encoding="utf-8"))
    every = {"core", "settle_frames", "orientation", "crop", "content", "bios", "buttons"}
    every |= {"reset", "ram"}
    assert set(raw) == every | {"episode", "core_options"}, "games/testcore covers every field"
    assert {"memory", "address", "scale", "swap16"} <= {k for v in raw["ram"].values() for k in v}
    assert {"bcd", "le", "be"} == {v["encoding"] for v in raw["ram"].values()}
    assert any("members" in bios for bios in raw["bios"]) and any("sha1" in b for b in raw["bios"])
    assert any("/" in bios["file"] for bios in raw["bios"]) and raw["content"]["parents"]
    config = _native.GameConfig.load(TESTCORE_TOML)
    assert (config.core, config.orientation, config.crop) == ("gth_testcore", 90, (0, 0, 320, 232))
    assert config.settle_frames == 1
    assert config.slots == ["shot", "bomb", "aux1", "aux2"] and config.n_actions == 9 * 16
    assert config.mapped_slots == config.slots
    assert config.reset_inputs == [(START, 2), (A, 3), (0, 1)] and config.reset_state is None
    assert config.ram_names == ["prng", "prng_swapped", "score", "score_tens"]
    assert config.core_options == {"gth_testcore_step": "4", "gth_testcore_lag": "on"}
    again = _native.GameConfig.parse(config.to_toml(), "round trip")
    assert again.to_toml() == config.to_toml()


def test_rom_fixture_finds_content_by_sha1(testcore_data: Path, rom, tmp_path: Path):
    assert rom("testcore") == tmp_path / "roms" / "sub" / "renamed.gthc"


def test_rom_fixture_skips_without_the_rom(rom, monkeypatch, tmp_path: Path):
    monkeypatch.delenv("GTH_ROM_DIR", raising=False)
    monkeypatch.setenv("GTH_CONFIG", str(tmp_path / "no-config.toml"))
    with pytest.raises(pytest.skip.Exception, match="no ROM directory"):
        rom("testcore")
    monkeypatch.setenv("GTH_ROM_DIR", str(tmp_path))
    with pytest.raises(pytest.skip.Exception, match=r"testcore\.gthc isn.t in"):
        rom("testcore")
    (tmp_path / "no-config.toml").write_text(f'rom_dir = "{tmp_path.as_posix()}/elsewhere"\n')
    monkeypatch.delenv("GTH_ROM_DIR")
    assert _native.rom_dir() == tmp_path / "elsewhere", "from the user config"


def test_core_fixture_skips_without_the_pinned_core(core, tmp_path: Path):
    with pytest.raises(pytest.skip.Exception, match=r"gunhed: no mednafen_pce_fast_libretro"):
        core("gunhed", tmp_path)
    library = core_library(tmp_path, "mednafen_pce_fast")
    library.parent.mkdir()
    library.write_bytes(b"not the pinned build")
    with pytest.raises(pytest.skip.Exception, match=r"isn.t the build cores\.lock pins"):
        core("gunhed", tmp_path)


def test_score_reward_matches_the_bcd_ram(testcore):
    env = testcore()
    _, info = env.reset()
    memory = env.memory(RETRO_MEMORY_SYSTEM_RAM)
    assert info["ram"]["score"] == bcd(memory[:4]) == 3, "the reset script held A 3 frames"
    assert info["ram"]["score_tens"] == 30
    assert info["ram"]["prng"] == int.from_bytes(memory[4:8], "little") != 0, "content seeds it"
    swapped = bytes([memory[5], memory[4], memory[7], memory[6]])
    assert info["ram"]["prng_swapped"] == int.from_bytes(swapped, "big")
    shot, aux2 = env.config.encode(0, 0b0001), env.config.encode(0, 0b1000)
    for action, expected in [(shot, 2.0), (shot, 2.0), (aux2, 0.0), (shot, 2.0)]:
        _, reward, _, _, info = env.step(action)
        assert reward == expected
        assert info["ram"]["score"] == bcd(env.memory(RETRO_MEMORY_SYSTEM_RAM)[:4])
    assert info["ram"]["score"] == 9 and info["episode_frame"] == 6 + 2 + 8
    assert env.observation_space.shape == (64, 64, 3), "RAM is never in the observation"


def test_terminate_fires_when_the_condition_holds(testcore):
    env = testcore(reward=lambda previous, current: current["score_tens"] - previous["score_tens"])
    env.reset()
    shot = env.config.encode(0, 1)
    steps = [env.step(shot) for _ in range(10)]
    assert [s[1] for s in steps] == [20.0] * 10, "a custom reward on another variable"
    scores = [s[4]["ram"]["score"] for s in steps]
    assert scores == list(range(5, 25, 2))
    assert [s[2] for s in steps] == [score >= 20 for score in scores], "score >= 20 && prng != 0"
    assert not any(s[3] for s in steps), "never truncated"


def test_orientation_crop_and_core_options_apply(testcore):
    env = testcore(frameskip=1, obs_size=(320, 232))
    env.reset()
    obs = env.step(env.config.encode(RIGHT, 0))[0]
    frame = obs.transpose(1, 0, 2)[::-1]  # undo the 90 degree turn
    assert frame.shape == (232, 320, 3), "cropped to 232 rows"
    white = (frame == 255).all(axis=2).nonzero()
    assert (white[0].min(), white[1].min()) == (112, 152 + 4), "gth_testcore_step = 4"


def test_from_game_names_what_is_missing(testcore_data: Path, testcore_path, monkeypatch, tmp_path):
    options = {
        "frameskip": 1,
        "obs_size": (8, 8),
        "action_format": "flat",
        "reward": score_delta,
        "core_path": testcore_path,
    }
    with pytest.raises(ConfigError, match=r"cannot read .*nogame"):
        RetroEnv.from_game("nogame", testcore_data, **options)
    empty = tmp_path / "empty"
    empty.mkdir()
    with pytest.raises(ConfigError, match=r"testcore\.gthc \(by SHA-1\) isn't in"):
        RetroEnv.from_game("testcore", testcore_data, rom_dir=empty, **options)
    parent = tmp_path / "roms" / "sub" / "gth_testcore_parent.zip"
    parent.rename(tmp_path / "roms" / "gth_testcore_parent.zip")
    with pytest.raises(ConfigError, match=r"parent sets missing .*\['gth_testcore_parent\.zip'\]"):
        RetroEnv.from_game("testcore", testcore_data, **options)
    (tmp_path / "roms" / "gth_testcore_parent.zip").rename(parent)
    bios_zip = testcore_data / "system" / "gth" / "gth_testcore_bios.zip"
    bios_zip.rename(testcore_data / "system" / "gth_testcore_bios.zip")
    with pytest.raises(ConfigError, match=r"BIOS files missing .*\['gth/gth_testcore_bios\.zip'\]"):
        RetroEnv.from_game("testcore", testcore_data, **options)
    monkeypatch.delenv("GTH_ROM_DIR")
    monkeypatch.setenv("GTH_CONFIG", str(tmp_path / "none.toml"))
    with pytest.raises(ConfigError, match="no ROM directory"):
        RetroEnv.from_game("testcore", testcore_data, **options)
    bios_zip.write_bytes(b"")
    del options["core_path"]
    with pytest.raises(CoreLoadError, match="gth_testcore_libretro"):
        RetroEnv.from_game("testcore", testcore_data, rom_dir=tmp_path / "roms", **options)


def test_from_game_finds_a_clone_through_its_parent(
    testcore_data: Path, testcore_path: Path, tmp_path: Path
):
    # A merged set: the clone has no zip of its own, only a place in its parent's.
    games = tmp_path / "games"
    (games / "merged").mkdir(parents=True)
    before, rest = TESTCORE_TOML.read_text(encoding="utf-8").split("[content]")
    after = rest.split("parents = [", 1)[1]
    content = (
        '[content]\nfile = "gth_testcore_clone.zip"\nin_parent = true\n'
        'members = [{ name = "clone.bin", size = 1, sha1 = "'
        + "0" * 40
        + '", crc32 = "00000000" }]\nparents = ['
    )
    (games / "merged" / "game.toml").write_text(before + content + after, encoding="utf-8")
    config = _native.GameConfig.load(games / "merged" / "game.toml")
    assert config.content_in_parent
    roms = tmp_path / "roms"
    clone = roms / "sub" / "gth_testcore_clone.zip"
    assert config.find_content(roms) == clone and not clone.exists()
    options = {
        "frameskip": 1,
        "obs_size": (8, 8),
        "action_format": "flat",
        "reward": score_delta,
        "core_path": testcore_path,
        "games_dir": games,
    }
    # gth-testcore reads content into memory, so it can't load a path that doesn't exist.
    with pytest.raises(ConfigError, match=r"in_parent\), which needs a core that loads content"):
        RetroEnv.from_game("merged", testcore_data, **options)
    (roms / "sub" / "gth_testcore_parent.zip").unlink()
    with pytest.raises(ConfigError, match=r"parent archive holding gth_testcore_clone\.zip"):
        RetroEnv.from_game("merged", testcore_data, **options)


def test_savestate_reset(testcore, testcore_data: Path, testcore_path: Path, tmp_path: Path):
    env = testcore()
    env.reset()
    for _ in range(4):
        env.step(env.config.encode(0, 1))
    state = env.save_state()
    env.close()
    games = tmp_path / "games"
    (games / "stated").mkdir(parents=True)
    before, rest = TESTCORE_TOML.read_text(encoding="utf-8").split("[reset]")
    after = rest.split("[ram.score]")[1]
    sha1 = hashlib.sha1(state).hexdigest()
    reset = f'[reset]\nstate = {{ file = "start.state", sha1 = "{sha1}" }}\n\n'
    toml = before + reset + "[ram.score]" + after
    (games / "stated" / "game.toml").write_text(toml, encoding="utf-8")

    def make() -> RetroEnv:
        return RetroEnv.from_game(
            "stated",
            testcore_data,
            frameskip=2,
            obs_size=(64, 64),
            action_format="dict",
            reward=score_delta,
            core_path=testcore_path,
            games_dir=games,
        )

    with pytest.raises(ConfigError, match="cannot read the reset state"):
        make()
    (testcore_data / "states" / "stated").mkdir(parents=True)
    state_file = testcore_data / "states" / "stated" / "start.state"
    state_file.write_bytes(state[:-1] + bytes([state[-1] ^ 1]))
    with pytest.raises(ConfigError, match=f"start.state: its SHA-1 is .*, not {sha1}"):
        make()
    state_file.write_bytes(state)
    stated = make()
    try:
        assert stated.config.reset_state == "start.state"
        _, info = stated.reset()
        assert info["ram"]["score"] == 11 and info["episode_frame"] == 2, "no script ran"
    finally:
        stated.close()
