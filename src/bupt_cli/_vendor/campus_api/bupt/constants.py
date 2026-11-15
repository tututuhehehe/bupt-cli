"""constants.py - Static constants, building metadata, and enumerations for BUPT services."""

from enum import Enum
from typing import NamedTuple


class BuildingMetadata(NamedTuple):
    """Immutable metadata for a campus dormitory building."""

    partment_name: str
    partment_id: str
    area_id: int
    default_floors: tuple[str, ...]


class CampusArea(int, Enum):
    """BUPT Campus Area ID."""

    XITUCHENG = 1  # 西土城/海淀校区
    SHAHE = 2  # 沙河校区


SHAHE_DEFAULT_FLOORS = ("1层", "2层", "3层", "4层", "5层", "6层", "强电")


class BuptBuilding(Enum):
    """Pre-defined standard building identifiers for BUPT dormitories.

    Each member embeds official metadata including exact partmentId, areaId, and floor hierarchy.
    """

    # --- 沙河校区雁北园 (Area 2) ---
    SHAHE_A = BuildingMetadata(
        "沙河校区雁北园A楼", "沙河校区雁北园A楼", 2, SHAHE_DEFAULT_FLOORS
    )
    SHAHE_B = BuildingMetadata(
        "沙河校区雁北园B楼", "沙河校区雁北园B楼", 2, SHAHE_DEFAULT_FLOORS
    )
    SHAHE_C = BuildingMetadata(
        "沙河校区雁北园C楼", "沙河校区雁北园C楼", 2, SHAHE_DEFAULT_FLOORS
    )
    SHAHE_D1 = BuildingMetadata(
        "沙河校区雁北园D1楼", "沙河校区雁北园D1楼", 2, SHAHE_DEFAULT_FLOORS
    )
    SHAHE_D2 = BuildingMetadata(
        "沙河校区雁北园D2楼", "沙河校区雁北园D2楼", 2, SHAHE_DEFAULT_FLOORS
    )
    SHAHE_E = BuildingMetadata(
        "沙河校区雁北园E楼", "沙河校区雁北园E楼", 2, SHAHE_DEFAULT_FLOORS
    )

    # --- 沙河校区雁南园 (Area 2) ---
    SHAHE_S2 = BuildingMetadata(
        "沙河校区雁南园S2楼", "沙河校区雁南园S2楼", 2, SHAHE_DEFAULT_FLOORS
    )
    SHAHE_S3 = BuildingMetadata(
        "沙河校区雁南园S3楼", "沙河校区雁南园S3楼", 2, SHAHE_DEFAULT_FLOORS
    )
    SHAHE_S4 = BuildingMetadata(
        "沙河校区雁南园S4楼", "沙河校区雁南园S4楼", 2, SHAHE_DEFAULT_FLOORS
    )
    SHAHE_S5 = BuildingMetadata(
        "沙河校区雁南园S5楼", "沙河校区雁南园S5楼", 2, SHAHE_DEFAULT_FLOORS
    )
    SHAHE_S6 = BuildingMetadata(
        "沙河校区雁南园S6楼", "沙河校区雁南园S6楼", 2, SHAHE_DEFAULT_FLOORS
    )

    # --- 西土城/海淀校区学字楼 (Area 1) ---
    HAIDIAN_XUE_1 = BuildingMetadata(
        "学一楼", "f8067fcf9c4f48c6b2f8e793d64b7b65", 1, ("-1", "1", "2", "3", "4", "5")
    )
    HAIDIAN_XUE_2 = BuildingMetadata(
        "学二楼", "f2224047ad13475eb7091f910ae27585", 1, ("1", "2", "3", "4", "5")
    )
    HAIDIAN_XUE_3 = BuildingMetadata(
        "学三楼", "f69719ee61454796b1574a314d136123", 1, ("1", "2", "3", "4", "5")
    )
    HAIDIAN_XUE_4 = BuildingMetadata(
        "学四楼", "c526d2367cd24fcdac2538975d1bec75", 1, ("1", "2", "3", "4", "5")
    )
    HAIDIAN_XUE_5 = BuildingMetadata(
        "学五楼", "a3d3473047464fba9196e224659cc377", 1, ("1", "2", "3", "4", "5")
    )
    HAIDIAN_XUE_6 = BuildingMetadata(
        "学六楼", "ac3a79c0e22e495096577d3aef1f5532", 1, ("1", "2", "3", "4", "5")
    )
    HAIDIAN_XUE_8 = BuildingMetadata(
        "学八楼", "f69719ee61454796b1574a314d136059", 1, ("1", "2", "3", "4", "5")
    )
    HAIDIAN_XUE_9 = BuildingMetadata(
        "学九楼", "1c89629ec4fa41ba9773b092e3851c08", 1, ("1", "2", "3", "4", "5")
    )
    HAIDIAN_XUE_10 = BuildingMetadata(
        "学十楼", "d5cf9743f0864692a18c25efb02bf16a", 1, ("1", "2", "3", "4", "5")
    )
    HAIDIAN_XUE_11 = BuildingMetadata(
        "学十一楼", "85a2e185790440e7978354838afb4f03", 1, ("1", "2", "3", "4", "5")
    )
    HAIDIAN_XUE_29 = BuildingMetadata(
        "90学二十九楼", "90bc4ce8f4084b4f8695ad1fa319b839", 1, ()
    )

    @property
    def partment_name(self) -> str:
        """Official name of the building on the electricity platform."""
        return self.value.partment_name

    @property
    def partment_id(self) -> str:
        """Official ID of the building for API requests."""
        return self.value.partment_id

    @property
    def area_id(self) -> int:
        """Campus area ID (1: 西土城, 2: 沙河)."""
        return self.value.area_id

    @property
    def default_floors(self) -> tuple[str, ...]:
        """Known floor list for this building."""
        return self.value.default_floors


class NoticeType(str, Enum):
    """BUPT information portal notice categories."""

    NOTICE = "notice"  # 通知公告 (wbtreeid: 1154)
    NEWS = "news"  # 校园新闻 (wbtreeid: 1221)
    RULES = "rules"  # 规章制度 (wbtreeid: 1536)
    WAYS = "ways"  # 办事指南 (wbtreeid: 1524)


NOTICE_TYPE_ID_MAP: dict[str, int] = {
    NoticeType.NOTICE.value: 1154,
    NoticeType.NEWS.value: 1221,
    NoticeType.RULES.value: 1536,
    NoticeType.WAYS.value: 1524,
    "notice": 1154,
    "news": 1221,
    "rules": 1536,
    "ways": 1524,
}

NOTICE_TYPE_NAME_MAP: dict[str, str] = {
    NoticeType.NOTICE.value: "通知公告",
    NoticeType.NEWS.value: "校园新闻",
    NoticeType.RULES.value: "规章制度",
    NoticeType.WAYS.value: "办事指南",
    "notice": "通知公告",
    "news": "校园新闻",
    "rules": "规章制度",
    "ways": "办事指南",
}
