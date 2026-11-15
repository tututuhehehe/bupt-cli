"""electricity.py - BUPT dormitory electricity query service (Sync and Async)."""

import re
from typing import Any, Optional, Union

import httpx

from bupt_cli._vendor.campus_api.bupt.auth import AsyncBuptSessionManager, BuptSessionManager
from bupt_cli._vendor.campus_api.bupt.cache import GLOBAL_CACHE, MetadataCache
from bupt_cli._vendor.campus_api.bupt.constants import BuptBuilding, CampusArea
from bupt_cli._vendor.campus_api.bupt.models import ElectricityInfo
from bupt_cli._vendor.campus_api.exceptions import (
    CampusAuthError,
    CampusNetworkError,
    CampusParseError,
    CampusResourceNotFoundError,
)

ELEC_API_BASE = "https://app.bupt.edu.cn/buptdf/wap/default"
ELEC_HEADERS = {
    "Referer": "https://app.bupt.edu.cn/buptdf/wap/default/chong",
    "X-Requested-With": "XMLHttpRequest",
}
_AUTH_ERROR_MARKERS = ("未登录", "请先登录", "登录失效", "登录过期")
_NOT_FOUND_MARKERS = ("未找到", "不存在", "无数据")


def _infer_floor_name(
    room_str: str, candidate_floors: tuple[str, ...], area_id: int
) -> Optional[str]:
    """Infer floor name from room number string against known building floors."""
    all_digits = re.findall(r"\d+", room_str)
    if not all_digits:
        return candidate_floors[0] if candidate_floors else None

    # Take the last group of digits (e.g. in "A楼101", take "101")
    room_digits = all_digits[-1]
    if len(room_digits) >= 3:
        floor_num = room_digits[:-2]  # e.g. 101 -> 1, 1204 -> 12
    else:
        floor_num = room_digits  # e.g. 1 -> 1

    # Shahe uses "5层", Haidian uses "5" or "1"
    suffix = "层" if area_id == 2 else ""
    target_name = f"{floor_num}{suffix}"

    for f in candidate_floors:
        if f == target_name or f.replace("层", "") == floor_num:
            return f

    return candidate_floors[0] if candidate_floors else None


def _normalize_room_key(value: Any) -> str:
    """Normalize room identifiers without discarding meaningful letters or digits."""
    return re.sub(r"[\s_-]+", "", str(value).strip()).upper()


def _room_match_score(room_name: Any, room_number: Any, query: str) -> int:
    """Score room matches so exact beats suffix, which beats containment."""
    normalized_query = _normalize_room_key(query)
    if not normalized_query:
        return 0

    candidates = tuple(
        candidate
        for candidate in (
            _normalize_room_key(room_name),
            _normalize_room_key(room_number),
        )
        if candidate
    )
    if any(candidate == normalized_query for candidate in candidates):
        return 3
    if any(
        candidate.endswith(normalized_query) or normalized_query.endswith(candidate)
        for candidate in candidates
    ):
        return 2
    if any(normalized_query in candidate for candidate in candidates):
        return 1
    return 0


def _extract_response_data(payload: Any, path: str) -> Any:
    """Validate the common electricity response envelope and its path-specific data."""
    if not isinstance(payload, dict):
        raise CampusParseError(f"Electricity API {path} response must be a JSON object")
    if "e" not in payload:
        raise CampusParseError(
            f"Electricity API {path} response missing status field 'e'"
        )

    status = str(payload["e"])
    msg = str(payload.get("m") or "")
    if any(marker in msg for marker in _AUTH_ERROR_MARKERS):
        raise CampusAuthError(f"Electricity API authentication failed: {msg}")
    if status != "0":
        if any(marker in msg for marker in _NOT_FOUND_MARKERS):
            raise CampusResourceNotFoundError(
                f"Electricity API {path} resource not found: {msg or status}"
            )
        raise CampusNetworkError(
            f"Electricity API {path} failed: {msg or f'code {status}'}"
        )

    envelope = payload.get("d")
    if not isinstance(envelope, dict) or "data" not in envelope:
        raise CampusParseError(
            f"Electricity API {path} response missing object 'd.data'"
        )

    data = envelope["data"]
    expected_type = dict if path == "/search" else list
    if not isinstance(data, expected_type):
        expected_name = "object" if expected_type is dict else "array"
        raise CampusParseError(
            f"Electricity API {path} field 'd.data' must be a JSON {expected_name}"
        )
    return data


class ElectricityService:
    """Synchronous service for querying BUPT dorm electricity balance and meter info."""

    def __init__(
        self,
        session_manager: BuptSessionManager,
        cache: Optional[MetadataCache] = None,
    ):
        self._session_mgr = session_manager
        self._cache = cache or GLOBAL_CACHE

    def _post(
        self, path: str, data: dict[str, Any], retry_on_auth_fail: bool = True
    ) -> Any:
        cli = self._session_mgr.prime_electricity_session()
        url = ELEC_API_BASE + path
        try:
            resp = cli.post(url, data=data, headers=ELEC_HEADERS)
            resp.raise_for_status()
            payload = resp.json()
        except httpx.HTTPStatusError as e:
            if e.response.status_code in (401, 403):
                if retry_on_auth_fail:
                    self._session_mgr.invalidate()
                    return self._post(path, data, retry_on_auth_fail=False)
                raise CampusAuthError(
                    f"Electricity API authentication failed (HTTP {e.response.status_code})"
                ) from e
            raise CampusNetworkError(
                f"Electricity API {path} network error: {e}"
            ) from e
        except httpx.HTTPError as e:
            raise CampusNetworkError(
                f"Electricity API {path} network error: {e}"
            ) from e
        except Exception as e:
            raise CampusParseError(
                f"Electricity API {path} returned invalid JSON: {e}"
            ) from e

        try:
            return _extract_response_data(payload, path)
        except CampusAuthError:
            if retry_on_auth_fail:
                self._session_mgr.invalidate()
                return self._post(path, data, retry_on_auth_fail=False)
            raise

    def get_buildings(
        self, area: CampusArea = CampusArea.SHAHE
    ) -> list[dict[str, Any]]:
        """Fetch all buildings registered in the given campus area."""
        return self._post("/part", {"areaid": int(area)})

    def get_floors(
        self, partment_id: str, area: CampusArea = CampusArea.SHAHE
    ) -> list[dict[str, Any]]:
        """Fetch all floors for a given building."""
        return self._post("/floor", {"partmentId": partment_id, "areaid": int(area)})

    def get_rooms(
        self,
        partment_id: str,
        floor_id: str,
        area: CampusArea = CampusArea.SHAHE,
    ) -> list[dict[str, Any]]:
        """Fetch all rooms for a given floor."""
        return self._post(
            "/drom",
            {"partmentId": partment_id, "floorId": floor_id, "areaid": int(area)},
        )

    def query(
        self,
        building: BuptBuilding,
        room: Union[int, str],
        floor: Optional[Union[int, str]] = None,
    ) -> ElectricityInfo:
        """Query electricity balance using static building metadata and O(1) cache.

        Args:
            building (BuptBuilding): Strongly typed BuptBuilding enum.
            room (Union[int, str]): Room number (e.g. 101 or "101").
            floor (Optional[Union[int, str]]): Explicit floor override. Inferred automatically if omitted.

        Returns:
            ElectricityInfo: Parsed meter details and remaining balance.
        """
        if not isinstance(building, BuptBuilding):
            raise TypeError(
                f"building must be an instance of BuptBuilding enum, got {type(building).__name__}"
            )

        partment_id = building.partment_id
        partment_name = building.partment_name
        area_id = building.area_id
        room_str = str(room).strip()
        if not room_str:
            raise ValueError("room must not be empty")

        # 1. Determine Floor Name
        if floor is not None:
            floor_name = str(floor).strip()
            if area_id == 2 and not floor_name.endswith("层") and floor_name.isdigit():
                floor_name = f"{floor_name}层"
        else:
            inferred = _infer_floor_name(room_str, building.default_floors, area_id)
            floor_name = inferred or (
                building.default_floors[0]
                if building.default_floors
                else ("1层" if area_id == int(CampusArea.SHAHE) else "1")
            )

        # 2. Check Cache for (drom_num, drom_name, floor_id)
        floor_id: Optional[str] = None
        cached = self._cache.get_room_meter(partment_id, floor_name, room_str)
        if cached:
            drom_num, drom_name, floor_id = cached
        else:
            # Cold path: resolve floor_id and room mapping
            floor_id = self._cache.get_floor_id(partment_id, floor_name)
            if not floor_id:
                floors = self.get_floors(partment_id, CampusArea(area_id))
                for f in floors:
                    fname = f.get("floorName", "")
                    fid = f.get("floorId", "")
                    self._cache.set_floor_id(partment_id, fname, fid)
                    if fname == floor_name:
                        floor_id = fid

            if not floor_id:
                raise CampusResourceNotFoundError(
                    f"Floor '{floor_name}' not found for building '{partment_name}'"
                )

            # Fetch rooms for this floor
            rooms = self.get_rooms(partment_id, floor_id, CampusArea(area_id))
            target_room = None
            best_score = 0
            for r in rooms:
                if not isinstance(r, dict):
                    raise CampusParseError(
                        "Electricity room list contains a non-object item"
                    )
                rname = str(r.get("dromName") or "")
                rnum = str(r.get("dromNum") or "")
                # Pre-populate cache for all rooms on this floor
                self._cache.set_room_meter(
                    partment_id, floor_name, rname, rnum, rname, floor_id
                )
                score = _room_match_score(rname, rnum, room_str)
                if score > best_score:
                    target_room = r
                    best_score = score

            if not target_room:
                raise CampusResourceNotFoundError(
                    f"Room '{room_str}' not found on floor '{floor_name}' in '{partment_name}'"
                )

            drom_num = str(target_room.get("dromNum") or "")
            drom_name = str(target_room.get("dromName") or room_str)
            if not drom_num:
                raise CampusParseError(
                    f"Room '{drom_name}' is missing its meter number"
                )
            self._cache.set_room_meter(
                partment_id, floor_name, room_str, drom_num, drom_name, floor_id
            )

        # 3. Fetch Meter Status (O(1) direct call)
        meter_data = self._post(
            "/search",
            {
                "partmentId": partment_id,
                "floorId": floor_id,
                "dromNumber": drom_num,
                "areaid": area_id,
            },
        )

        return ElectricityInfo.from_api_response(
            data=meter_data,
            building=partment_name,
            floor=floor_name,
            room=drom_name,
            meter_number=drom_num,
        )


class AsyncElectricityService:
    """Asynchronous service for querying BUPT dorm electricity balance and meter info."""

    def __init__(
        self,
        session_manager: AsyncBuptSessionManager,
        cache: Optional[MetadataCache] = None,
    ):
        self._session_mgr = session_manager
        self._cache = cache or GLOBAL_CACHE

    async def _post(
        self, path: str, data: dict[str, Any], retry_on_auth_fail: bool = True
    ) -> Any:
        cli = await self._session_mgr.prime_electricity_session()
        url = ELEC_API_BASE + path
        try:
            resp = await cli.post(url, data=data, headers=ELEC_HEADERS)
            resp.raise_for_status()
            payload = resp.json()
        except httpx.HTTPStatusError as e:
            if e.response.status_code in (401, 403):
                if retry_on_auth_fail:
                    self._session_mgr.invalidate()
                    return await self._post(path, data, retry_on_auth_fail=False)
                raise CampusAuthError(
                    f"Electricity API authentication failed (HTTP {e.response.status_code})"
                ) from e
            raise CampusNetworkError(
                f"Electricity API {path} network error: {e}"
            ) from e
        except httpx.HTTPError as e:
            raise CampusNetworkError(
                f"Electricity API {path} network error: {e}"
            ) from e
        except Exception as e:
            raise CampusParseError(
                f"Electricity API {path} returned invalid JSON: {e}"
            ) from e

        try:
            return _extract_response_data(payload, path)
        except CampusAuthError:
            if retry_on_auth_fail:
                self._session_mgr.invalidate()
                return await self._post(path, data, retry_on_auth_fail=False)
            raise

    async def get_buildings(
        self, area: CampusArea = CampusArea.SHAHE
    ) -> list[dict[str, Any]]:
        """Fetch all buildings registered in the given campus area asynchronously."""
        return await self._post("/part", {"areaid": int(area)})

    async def get_floors(
        self, partment_id: str, area: CampusArea = CampusArea.SHAHE
    ) -> list[dict[str, Any]]:
        """Fetch all floors for a given building asynchronously."""
        return await self._post(
            "/floor", {"partmentId": partment_id, "areaid": int(area)}
        )

    async def get_rooms(
        self,
        partment_id: str,
        floor_id: str,
        area: CampusArea = CampusArea.SHAHE,
    ) -> list[dict[str, Any]]:
        """Fetch all rooms for a given floor asynchronously."""
        return await self._post(
            "/drom",
            {"partmentId": partment_id, "floorId": floor_id, "areaid": int(area)},
        )

    async def query(
        self,
        building: BuptBuilding,
        room: Union[int, str],
        floor: Optional[Union[int, str]] = None,
    ) -> ElectricityInfo:
        """Query electricity balance asynchronously with O(1) cache."""
        if not isinstance(building, BuptBuilding):
            raise TypeError(
                f"building must be an instance of BuptBuilding enum, got {type(building).__name__}"
            )

        partment_id = building.partment_id
        partment_name = building.partment_name
        area_id = building.area_id
        room_str = str(room).strip()
        if not room_str:
            raise ValueError("room must not be empty")

        if floor is not None:
            floor_name = str(floor).strip()
            if area_id == 2 and not floor_name.endswith("层") and floor_name.isdigit():
                floor_name = f"{floor_name}层"
        else:
            inferred = _infer_floor_name(room_str, building.default_floors, area_id)
            floor_name = inferred or (
                building.default_floors[0]
                if building.default_floors
                else ("1层" if area_id == int(CampusArea.SHAHE) else "1")
            )

        floor_id: Optional[str] = None
        cached = self._cache.get_room_meter(partment_id, floor_name, room_str)
        if cached:
            drom_num, drom_name, floor_id = cached
        else:
            floor_id = self._cache.get_floor_id(partment_id, floor_name)
            if not floor_id:
                floors = await self.get_floors(partment_id, CampusArea(area_id))
                for f in floors:
                    fname = f.get("floorName", "")
                    fid = f.get("floorId", "")
                    self._cache.set_floor_id(partment_id, fname, fid)
                    if fname == floor_name:
                        floor_id = fid

            if not floor_id:
                raise CampusResourceNotFoundError(
                    f"Floor '{floor_name}' not found for building '{partment_name}'"
                )

            rooms = await self.get_rooms(partment_id, floor_id, CampusArea(area_id))
            target_room = None
            best_score = 0
            for r in rooms:
                if not isinstance(r, dict):
                    raise CampusParseError(
                        "Electricity room list contains a non-object item"
                    )
                rname = str(r.get("dromName") or "")
                rnum = str(r.get("dromNum") or "")
                self._cache.set_room_meter(
                    partment_id, floor_name, rname, rnum, rname, floor_id
                )
                score = _room_match_score(rname, rnum, room_str)
                if score > best_score:
                    target_room = r
                    best_score = score

            if not target_room:
                raise CampusResourceNotFoundError(
                    f"Room '{room_str}' not found on floor '{floor_name}' in '{partment_name}'"
                )

            drom_num = str(target_room.get("dromNum") or "")
            drom_name = str(target_room.get("dromName") or room_str)
            if not drom_num:
                raise CampusParseError(
                    f"Room '{drom_name}' is missing its meter number"
                )
            self._cache.set_room_meter(
                partment_id, floor_name, room_str, drom_num, drom_name, floor_id
            )

        meter_data = await self._post(
            "/search",
            {
                "partmentId": partment_id,
                "floorId": floor_id,
                "dromNumber": drom_num,
                "areaid": area_id,
            },
        )

        return ElectricityInfo.from_api_response(
            data=meter_data,
            building=partment_name,
            floor=floor_name,
            room=drom_name,
            meter_number=drom_num,
        )
