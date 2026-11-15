"""Offline tests for the electricity default-dorm resolution and buildings list.

The network client is always replaced by a stub: these tests never touch
app.bupt.edu.cn and never need credentials.
"""

import json

import pytest

from bupt_cli import cli, commands
from bupt_cli._vendor.campus_api.bupt.constants import BuptBuilding
from bupt_cli._vendor.campus_api.bupt.models import ElectricityInfo
from bupt_cli.errors import UsageError

DORM_ENV_VARS = ("BUPT_DORM_BUILDING", "BUPT_DORM_ROOM", "BUPT_DORM_FLOOR")


class FakeClient:
    def __init__(self):
        self.queries = []
        self.area_queries = []

    def get_electricity(self, building, room, floor=None):
        self.queries.append({"building": building, "room": room, "floor": floor})
        return ElectricityInfo(
            surplus=42.5,
            price=0.48,
            total_consumed_kwh=88.54,
            time="2026-10-21 03:10:51",
            building=building.partment_name,
            floor=str(floor) if floor else "4",
            room=str(room),
            meter_number="meter-1",
        )

    def get_electricity_buildings(self, area):
        self.area_queries.append(area)
        return [{"partmentName": "学五楼", "partmentId": "id-5", "areaId": int(area)}]


@pytest.fixture
def clean_dorm_env(monkeypatch, tmp_path):
    """Isolate config dir and drop dorm env vars."""
    monkeypatch.setenv("BUPT_CLI_CONFIG_DIR", str(tmp_path / "config"))
    for name in DORM_ENV_VARS:
        monkeypatch.delenv(name, raising=False)
    return tmp_path


@pytest.fixture
def fake_client(monkeypatch):
    client = FakeClient()
    monkeypatch.setattr(commands, "_client", lambda _session, **_: client)
    monkeypatch.setattr(commands, "_session", lambda _args, **_: object())
    monkeypatch.setattr(commands, "_persist", lambda _session: None)
    return client


def _argv(**kwargs):
    values = {"building": None, "room": None, "floor": None}
    values.update(kwargs)
    return type("Args", (), values)()


def _write_config(tmp_path, payload):
    path = tmp_path / "config" / "config.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload), encoding="utf-8")
    return path


def test_default_dorm_is_xitucheng_xue1_101(clean_dorm_env):
    resolved = commands.resolve_dorm(_argv())
    assert resolved["building"] is BuptBuilding.HAIDIAN_XUE_1
    assert resolved["room"] == "101"
    assert resolved["floor"] is None
    assert resolved["source"] == "default"


def test_default_dorm_query_shape(clean_dorm_env, fake_client, capsys):
    assert cli.main(["electricity"]) == 0
    payload = json.loads(capsys.readouterr().out)

    assert payload["ok"] is True
    assert payload["source"] == "default"
    assert payload["room"] == "101"
    assert payload["building"] == "学一楼"
    assert payload["surplus"] == 42.5
    assert payload["low_balance"] is False
    assert payload["threshold"] == 10.0
    query = fake_client.queries[0]
    assert query["building"] is BuptBuilding.HAIDIAN_XUE_1
    assert query["room"] == "101"
    assert query["floor"] is None
    assert fake_client.area_queries == []


def test_env_overrides_default(clean_dorm_env, monkeypatch):
    monkeypatch.setenv("BUPT_DORM_BUILDING", "SHAHE_A")
    monkeypatch.setenv("BUPT_DORM_ROOM", "102")
    monkeypatch.setenv("BUPT_DORM_FLOOR", "1层")
    resolved = commands.resolve_dorm(_argv())
    assert resolved["building"] is BuptBuilding.SHAHE_A
    assert resolved["room"] == "102"
    assert resolved["floor"] == "1层"
    assert resolved["source"] == "env"


def test_config_used_when_no_flag_or_env(clean_dorm_env):
    _write_config(
        clean_dorm_env,
        {"dorm": {"building": "学5", "room": "202", "floor": "2"}},
    )
    resolved = commands.resolve_dorm(_argv())
    assert resolved["building"] is BuptBuilding.HAIDIAN_XUE_5
    assert resolved["room"] == "202"
    assert resolved["floor"] == "2"
    assert resolved["source"] == "config"


def test_env_beats_config(clean_dorm_env, monkeypatch):
    _write_config(clean_dorm_env, {"dorm": {"building": "学五楼", "room": "202"}})
    monkeypatch.setenv("BUPT_DORM_ROOM", "414")
    resolved = commands.resolve_dorm(_argv())
    assert resolved["room"] == "414"
    assert resolved["source"] == "env"


@pytest.mark.parametrize("mode", ["flag", "env", "config"])
def test_cli_source_reflects_room_origin(
    clean_dorm_env, fake_client, monkeypatch, capsys, mode
):
    argv = ["electricity"]
    if mode == "flag":
        argv += ["--building", "学五楼", "--room", "202"]
    elif mode == "env":
        monkeypatch.setenv("BUPT_DORM_BUILDING", "学五楼")
        monkeypatch.setenv("BUPT_DORM_ROOM", "202")
    else:
        _write_config(clean_dorm_env, {"dorm": {"building": "学五楼", "room": "202"}})

    assert cli.main(argv) == 0
    payload = json.loads(capsys.readouterr().out)
    assert payload["source"] == mode
    assert payload["room"] == "202"
    assert payload["building"] == "学五楼"


def test_flags_beat_env_and_config(clean_dorm_env, monkeypatch):
    _write_config(clean_dorm_env, {"dorm": {"building": "学五楼", "room": "202"}})
    monkeypatch.setenv("BUPT_DORM_BUILDING", "学五楼")
    monkeypatch.setenv("BUPT_DORM_ROOM", "414")
    resolved = commands.resolve_dorm(
        _argv(building="沙河校区雁北园A楼", room="102", floor="1")
    )
    assert resolved["building"] is BuptBuilding.SHAHE_A
    assert resolved["room"] == "102"
    assert resolved["floor"] == "1"
    assert resolved["source"] == "flag"


@pytest.mark.parametrize(
    "name, expected",
    [
        ("学五楼", BuptBuilding.HAIDIAN_XUE_5),
        ("学5楼", BuptBuilding.HAIDIAN_XUE_5),
        ("学5", BuptBuilding.HAIDIAN_XUE_5),
        ("5", BuptBuilding.HAIDIAN_XUE_5),
        ("学十一楼", BuptBuilding.HAIDIAN_XUE_11),
        ("学29楼", BuptBuilding.HAIDIAN_XUE_29),
        ("SHAHE_A", BuptBuilding.SHAHE_A),
        ("shahe_a", BuptBuilding.SHAHE_A),
        ("沙河校区雁北园A楼", BuptBuilding.SHAHE_A),
    ],
)
def test_building_aliases(name, expected):
    assert commands._resolve_building(name) is expected


def test_unknown_building_is_usage_error():
    with pytest.raises(UsageError):
        commands._resolve_building("不存在楼")


def test_building_without_room_is_usage_error(clean_dorm_env, fake_client, capsys):
    code = cli.main(["electricity", "--building", "学五楼"])
    payload = json.loads(capsys.readouterr().err)
    assert code == 2
    assert payload["error"]["type"] == "usage"
    assert "--room is required" in payload["error"]["message"]
    assert fake_client.queries == []


def test_area_is_rejected_on_top_level_electricity(clean_dorm_env, fake_client, capsys):
    code = cli.main(["electricity", "--area", "shahe"])
    payload = json.loads(capsys.readouterr().err)
    assert code == 2
    assert payload["error"]["type"] == "usage"
    assert "--area is only valid" in payload["error"]["message"]
    assert "bupt electricity buildings --area" in payload["error"]["hint"]
    assert fake_client.queries == []
    assert fake_client.area_queries == []


def test_bad_config_json_is_usage_error(clean_dorm_env, fake_client, capsys):
    path = clean_dorm_env / "config" / "config.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("{not valid json", encoding="utf-8")

    code = cli.main(["electricity"])
    payload = json.loads(capsys.readouterr().err)
    assert code == 2
    assert payload["error"]["type"] == "usage"
    assert "config" in payload["error"]["message"]
    assert fake_client.queries == []


def test_non_object_dorm_config_is_usage_error(clean_dorm_env):
    _write_config(clean_dorm_env, {"dorm": "not-an-object"})
    with pytest.raises(UsageError):
        commands.resolve_dorm(_argv())


def test_unreadable_config_is_usage_error_not_json_hint(clean_dorm_env):
    # A directory at config.json raises OSError (IsADirectoryError) on open;
    # it must not be reported as a JSON syntax problem.
    path = clean_dorm_env / "config" / "config.json"
    path.mkdir(parents=True)
    with pytest.raises(UsageError) as excinfo:
        commands.resolve_dorm(_argv())
    assert "cannot read config file" in excinfo.value.message
    assert "permissions" in excinfo.value.hint


def test_buildings_subcommand_ignores_broken_config(
    clean_dorm_env, fake_client, capsys
):
    """Listing only needs upstream metadata; it deliberately skips config.json.

    A broken config blocks queries (usage error) but must not block listing
    buildings, which never uses the dorm defaults.
    """
    path = clean_dorm_env / "config" / "config.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("{not valid json", encoding="utf-8")

    assert cli.main(["electricity", "buildings"]) == 0
    payload = json.loads(capsys.readouterr().out)
    assert payload["ok"] is True
    assert payload["count"] == 1
    assert fake_client.area_queries[0].name == "SHAHE"


def test_buildings_subcommand_defaults_to_shahe(clean_dorm_env, fake_client, capsys):
    assert cli.main(["electricity", "buildings"]) == 0
    payload = json.loads(capsys.readouterr().out)

    assert payload["ok"] is True
    assert payload["area"] == "shahe"
    assert payload["count"] == 1
    assert payload["buildings"][0]["partmentName"] == "学五楼"
    assert fake_client.area_queries[0].name == "SHAHE"


def test_buildings_subcommand_accepts_xitucheng(clean_dorm_env, fake_client, capsys):
    assert cli.main(["electricity", "buildings", "--area", "xitucheng"]) == 0
    payload = json.loads(capsys.readouterr().out)
    assert payload["area"] == "xitucheng"
    assert fake_client.area_queries[0].name == "XITUCHENG"
    assert fake_client.queries == []


def test_buildings_subcommand_rejects_bad_area(clean_dorm_env, fake_client, capsys):
    code = cli.main(["electricity", "buildings", "--area", "mars"])
    payload = json.loads(capsys.readouterr().err)
    assert code == 2
    assert payload["error"]["type"] == "usage"
