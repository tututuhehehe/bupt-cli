use crate::cli::{CliError, CliResult};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const DEFAULT_BUILDING: &str = "学一楼";
const DEFAULT_ROOM: &str = "101";
const SHAHE_FLOORS: &[&str] = &["1层", "2层", "3层", "4层", "5层", "6层", "强电"];
const HAIDIAN_FLOORS: &[&str] = &["1", "2", "3", "4", "5"];
const HAIDIAN_XUE_1_FLOORS: &[&str] = &["-1", "1", "2", "3", "4", "5"];

#[derive(Clone, Copy)]
pub(super) struct Building {
    key: &'static str,
    pub(super) name: &'static str,
    partment_id: &'static str,
    area_id: i32,
    default_floors: &'static [&'static str],
}

impl Building {
    pub(super) fn partment_id(self) -> &'static str {
        self.partment_id
    }

    pub(super) fn area_id(self) -> i32 {
        self.area_id
    }

    pub(super) fn default_floors(self) -> &'static [&'static str] {
        self.default_floors
    }

    pub(super) fn area(self) -> Area {
        match self.area_id {
            2 => Area::Shahe,
            _ => Area::Xitucheng,
        }
    }
}

const BUILDINGS: &[Building] = &[
    Building {
        key: "SHAHE_A",
        name: "沙河校区雁北园A楼",
        partment_id: "沙河校区雁北园A楼",
        area_id: 2,
        default_floors: SHAHE_FLOORS,
    },
    Building {
        key: "SHAHE_B",
        name: "沙河校区雁北园B楼",
        partment_id: "沙河校区雁北园B楼",
        area_id: 2,
        default_floors: SHAHE_FLOORS,
    },
    Building {
        key: "SHAHE_C",
        name: "沙河校区雁北园C楼",
        partment_id: "沙河校区雁北园C楼",
        area_id: 2,
        default_floors: SHAHE_FLOORS,
    },
    Building {
        key: "SHAHE_D1",
        name: "沙河校区雁北园D1楼",
        partment_id: "沙河校区雁北园D1楼",
        area_id: 2,
        default_floors: SHAHE_FLOORS,
    },
    Building {
        key: "SHAHE_D2",
        name: "沙河校区雁北园D2楼",
        partment_id: "沙河校区雁北园D2楼",
        area_id: 2,
        default_floors: SHAHE_FLOORS,
    },
    Building {
        key: "SHAHE_E",
        name: "沙河校区雁北园E楼",
        partment_id: "沙河校区雁北园E楼",
        area_id: 2,
        default_floors: SHAHE_FLOORS,
    },
    Building {
        key: "SHAHE_S2",
        name: "沙河校区雁南园S2楼",
        partment_id: "沙河校区雁南园S2楼",
        area_id: 2,
        default_floors: SHAHE_FLOORS,
    },
    Building {
        key: "SHAHE_S3",
        name: "沙河校区雁南园S3楼",
        partment_id: "沙河校区雁南园S3楼",
        area_id: 2,
        default_floors: SHAHE_FLOORS,
    },
    Building {
        key: "SHAHE_S4",
        name: "沙河校区雁南园S4楼",
        partment_id: "沙河校区雁南园S4楼",
        area_id: 2,
        default_floors: SHAHE_FLOORS,
    },
    Building {
        key: "SHAHE_S5",
        name: "沙河校区雁南园S5楼",
        partment_id: "沙河校区雁南园S5楼",
        area_id: 2,
        default_floors: SHAHE_FLOORS,
    },
    Building {
        key: "SHAHE_S6",
        name: "沙河校区雁南园S6楼",
        partment_id: "沙河校区雁南园S6楼",
        area_id: 2,
        default_floors: SHAHE_FLOORS,
    },
    Building {
        key: "HAIDIAN_XUE_1",
        name: "学一楼",
        partment_id: "f8067fcf9c4f48c6b2f8e793d64b7b65",
        area_id: 1,
        default_floors: HAIDIAN_XUE_1_FLOORS,
    },
    Building {
        key: "HAIDIAN_XUE_2",
        name: "学二楼",
        partment_id: "f2224047ad13475eb7091f910ae27585",
        area_id: 1,
        default_floors: HAIDIAN_FLOORS,
    },
    Building {
        key: "HAIDIAN_XUE_3",
        name: "学三楼",
        partment_id: "f69719ee61454796b1574a314d136123",
        area_id: 1,
        default_floors: HAIDIAN_FLOORS,
    },
    Building {
        key: "HAIDIAN_XUE_4",
        name: "学四楼",
        partment_id: "c526d2367cd24fcdac2538975d1bec75",
        area_id: 1,
        default_floors: HAIDIAN_FLOORS,
    },
    Building {
        key: "HAIDIAN_XUE_5",
        name: "学五楼",
        partment_id: "a3d3473047464fba9196e224659cc377",
        area_id: 1,
        default_floors: HAIDIAN_FLOORS,
    },
    Building {
        key: "HAIDIAN_XUE_6",
        name: "学六楼",
        partment_id: "ac3a79c0e22e495096577d3aef1f5532",
        area_id: 1,
        default_floors: HAIDIAN_FLOORS,
    },
    Building {
        key: "HAIDIAN_XUE_8",
        name: "学八楼",
        partment_id: "f69719ee61454796b1574a314d136059",
        area_id: 1,
        default_floors: HAIDIAN_FLOORS,
    },
    Building {
        key: "HAIDIAN_XUE_9",
        name: "学九楼",
        partment_id: "1c89629ec4fa41ba9773b092e3851c08",
        area_id: 1,
        default_floors: HAIDIAN_FLOORS,
    },
    Building {
        key: "HAIDIAN_XUE_10",
        name: "学十楼",
        partment_id: "d5cf9743f0864692a18c25efb02bf16a",
        area_id: 1,
        default_floors: HAIDIAN_FLOORS,
    },
    Building {
        key: "HAIDIAN_XUE_11",
        name: "学十一楼",
        partment_id: "85a2e185790440e7978354838afb4f03",
        area_id: 1,
        default_floors: HAIDIAN_FLOORS,
    },
    Building {
        key: "HAIDIAN_XUE_29",
        name: "90学二十九楼",
        partment_id: "90bc4ce8f4084b4f8695ad1fa319b839",
        area_id: 1,
        default_floors: &[],
    },
];
#[derive(Clone, Copy)]
pub(super) enum Area {
    Shahe,
    Xitucheng,
}

impl Area {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Shahe => "shahe",
            Self::Xitucheng => "xitucheng",
        }
    }
}

#[derive(Default)]
pub(super) struct DormFlags {
    pub(super) building: Option<String>,
    pub(super) room: Option<String>,
    pub(super) floor: Option<String>,
}

#[derive(Default)]
pub(super) struct DormEnv {
    pub(super) building: Option<String>,
    pub(super) room: Option<String>,
    pub(super) floor: Option<String>,
}

pub(super) struct Dorm {
    pub(super) building: &'static Building,
    pub(super) room: String,
    pub(super) floor: Option<String>,
    pub(super) source: &'static str,
}

pub(super) fn resolve_area(value: &str) -> CliResult<Area> {
    match value.trim().to_ascii_lowercase().as_str() {
        "shahe" | "沙河" | "2" => Ok(Area::Shahe),
        "xitucheng" | "haidian" | "西土城" | "海淀" | "1" => Ok(Area::Xitucheng),
        _ => Err(CliError::new(
            2,
            "usage",
            "--area must be 'shahe' or 'xitucheng'",
            "try `bupt electricity buildings --area shahe` or `bupt electricity buildings --area xitucheng`",
        )),
    }
}

pub(super) fn dorm_env() -> DormEnv {
    DormEnv {
        building: non_empty_env("BUPT_DORM_BUILDING"),
        room: non_empty_env("BUPT_DORM_ROOM"),
        floor: non_empty_env("BUPT_DORM_FLOOR"),
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(super) fn resolve_dorm(flags: &DormFlags, env: &DormEnv, config_dir: &Path) -> CliResult<Dorm> {
    let raw_building = flags.building.as_deref().unwrap_or("");
    let raw_room = flags.room.as_deref().unwrap_or("");
    if !raw_building.is_empty() && raw_room.is_empty() {
        return Err(CliError::new(
            2,
            "usage",
            "--room is required when --building is given",
            "e.g. `bupt electricity --building 学一楼 --room 101`",
        ));
    }

    let config = load_dorm_config(config_dir)?;
    let config_building = config.get("building");
    let config_room = config.get("room");
    let config_floor = config.get("floor");

    let (building_value, _) = choose(
        flags.building.as_deref(),
        env.building.as_deref(),
        config_building.map(String::as_str),
    );
    let (room, room_source) = choose(
        flags.room.as_deref(),
        env.room.as_deref(),
        config_room.map(String::as_str),
    );
    let (floor, _) = choose(
        flags.floor.as_deref(),
        env.floor.as_deref(),
        config_floor.map(String::as_str),
    );

    let building_value = building_value
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_BUILDING.to_string());
    let room = room.unwrap_or_else(|| DEFAULT_ROOM.to_string());
    Ok(Dorm {
        building: resolve_building(&building_value)?,
        room,
        floor,
        source: room_source.unwrap_or("default"),
    })
}

fn choose(
    flag: Option<&str>,
    env: Option<&str>,
    config: Option<&str>,
) -> (Option<String>, Option<&'static str>) {
    if let Some(value) = flag.filter(|value| !value.is_empty()) {
        (Some(value.trim().to_string()), Some("flag"))
    } else if let Some(value) = env.filter(|value| !value.trim().is_empty()) {
        (Some(value.trim().to_string()), Some("env"))
    } else if let Some(value) = config.map(str::trim).filter(|value| !value.is_empty()) {
        (Some(value.to_string()), Some("config"))
    } else {
        (None, None)
    }
}

fn load_dorm_config(config_dir: &Path) -> CliResult<BTreeMap<String, String>> {
    let path = config_dir.join("config.json");
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let text = fs::read_to_string(&path).map_err(|error| {
        CliError::new(
            2,
            "usage",
            format!("cannot read config file {}: {error}", path.display()),
            "check file permissions/readability",
        )
    })?;
    let payload: Value = serde_json::from_str(&text).map_err(|error| {
        CliError::new(
            2,
            "usage",
            format!("cannot parse config file {}: {error}", path.display()),
            "fix the JSON content",
        )
    })?;
    let Value::Object(object) = payload else {
        return Err(CliError::usage(format!(
            "config file {} must contain a JSON object",
            path.display()
        )));
    };
    let dorm = match object.get("dorm") {
        None | Some(Value::Null) => return Ok(BTreeMap::new()),
        Some(Value::Object(dorm)) => dorm,
        Some(_) => {
            return Err(CliError::usage(format!(
                "config file {}: 'dorm' must be a JSON object",
                path.display()
            )));
        }
    };
    let mut values = BTreeMap::new();
    for key in ["building", "room", "floor"] {
        let Some(value) = dorm.get(key) else {
            continue;
        };
        let text = match value {
            Value::Null => continue,
            Value::String(text) => text.trim().to_string(),
            other => other.to_string(),
        };
        if !text.is_empty() {
            values.insert(key.to_string(), text);
        }
    }
    Ok(values)
}

fn resolve_building(value: &str) -> CliResult<&'static Building> {
    let raw = value.trim();
    let key = raw.to_ascii_uppercase().replace('-', "_");
    if let Some(building) = BUILDINGS
        .iter()
        .find(|building| building.key == key || building.name == raw)
    {
        return Ok(building);
    }
    if let Some(building) = BUILDINGS
        .iter()
        .find(|building| matches_building_alias(building, raw))
    {
        return Ok(building);
    }
    let choices = BUILDINGS
        .iter()
        .map(|building| building.key)
        .collect::<Vec<_>>()
        .join(", ");
    Err(CliError::new(
        2,
        "usage",
        format!("unknown building '{value}'"),
        format!(
            "known buildings: {choices}; run `bupt electricity buildings --area xitucheng` (or --area shahe) for the upstream list"
        ),
    ))
}

fn matches_building_alias(building: &Building, value: &str) -> bool {
    let Some(number) = chinese_building_number(building.name) else {
        return false;
    };
    value == format!("学{number}楼")
        || value == format!("学{number}")
        || value == format!("{number}楼")
        || value == number.to_string()
}

fn chinese_building_number(name: &str) -> Option<u32> {
    let start = name.find('学')? + '学'.len_utf8();
    let rest = &name[start..];
    let end = rest.find('楼')?;
    chinese_number(&rest[..end])
}

fn chinese_number(text: &str) -> Option<u32> {
    fn digit(character: char) -> Option<u32> {
        Some(match character {
            '零' => 0,
            '一' => 1,
            '二' => 2,
            '三' => 3,
            '四' => 4,
            '五' => 5,
            '六' => 6,
            '七' => 7,
            '八' => 8,
            '九' => 9,
            _ => return None,
        })
    }
    if text.is_empty() {
        return None;
    }
    if let Some((left, right)) = text.split_once('十') {
        let tens = if left.is_empty() {
            1
        } else {
            let mut chars = left.chars();
            let value = digit(chars.next()?)?;
            if chars.next().is_some() {
                return None;
            }
            value
        };
        let ones = if right.is_empty() {
            0
        } else {
            let mut chars = right.chars();
            let value = digit(chars.next()?)?;
            if chars.next().is_some() {
                return None;
            }
            value
        };
        return Some(tens * 10 + ones);
    }
    let mut chars = text.chars();
    let value = digit(chars.next()?)?;
    if chars.next().is_some() {
        return None;
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn dorm_config_defaults_and_config_values_are_resolved_locally() {
        let directory = tempfile::tempdir().unwrap();
        let defaults =
            resolve_dorm(&DormFlags::default(), &DormEnv::default(), directory.path()).unwrap();
        assert_eq!(defaults.building.name, "学一楼");
        assert_eq!(defaults.room, "101");
        assert_eq!(defaults.floor, None);
        assert_eq!(defaults.source, "default");

        fs::write(
            directory.path().join("config.json"),
            r#"{"dorm":{"building":"学五楼","room":" 202 ","floor":"2"}}"#,
        )
        .unwrap();
        let configured =
            resolve_dorm(&DormFlags::default(), &DormEnv::default(), directory.path()).unwrap();
        assert_eq!(configured.building.name, "学五楼");
        assert_eq!(configured.room, "202");
        assert_eq!(configured.floor.as_deref(), Some("2"));
        assert_eq!(configured.source, "config");
    }

    #[test]
    fn building_metadata_matches_the_python_constants() {
        let xue1 = resolve_building("学一楼").unwrap();
        assert_eq!(xue1.area_id(), 1);
        assert_eq!(xue1.partment_id(), "f8067fcf9c4f48c6b2f8e793d64b7b65");
        assert_eq!(xue1.default_floors(), &["-1", "1", "2", "3", "4", "5"]);

        let shahe = resolve_building("SHAHE_A").unwrap();
        assert_eq!(shahe.area_id(), 2);
        assert_eq!(shahe.partment_id(), "沙河校区雁北园A楼");
        assert_eq!(
            shahe.default_floors(),
            &["1层", "2层", "3层", "4层", "5层", "6层", "强电"]
        );

        let xue29 = resolve_building("学29楼").unwrap();
        assert_eq!(xue29.name, "90学二十九楼");
        assert_eq!(xue29.partment_id(), "90bc4ce8f4084b4f8695ad1fa319b839");
        assert!(xue29.default_floors().is_empty());
    }

    #[test]
    fn dorm_flags_win_over_environment_and_config() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("config.json"),
            r#"{"dorm":{"building":"不存在楼","room":"202"}}"#,
        )
        .unwrap();
        let flags = DormFlags {
            building: Some("沙河校区雁北园A楼".into()),
            room: Some("102".into()),
            floor: Some("1层".into()),
        };
        let env = DormEnv {
            building: Some("不存在楼".into()),
            room: Some("414".into()),
            floor: None,
        };
        let resolved = resolve_dorm(&flags, &env, directory.path()).unwrap();
        assert_eq!(resolved.building.name, "沙河校区雁北园A楼");
        assert_eq!(resolved.room, "102");
        assert_eq!(resolved.floor.as_deref(), Some("1层"));
        assert_eq!(resolved.source, "flag");
    }
}
