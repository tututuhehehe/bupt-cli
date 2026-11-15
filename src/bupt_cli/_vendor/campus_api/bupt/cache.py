"""cache.py - Thread-safe in-memory metadata cache for BUPT campus services."""

import threading
from typing import Optional


class MetadataCache:
    """Thread-safe cache for room-to-meter mappings and campus metadata."""

    def __init__(self) -> None:
        self._lock = threading.Lock()
        # Key: (partment_id, floor_name, room_query_str) -> (drom_num, drom_name, floor_id)
        self._room_meters: dict[tuple[str, str, str], tuple[str, str, str]] = {}
        # Key: (partment_id, floor_name) -> floor_id
        self._floor_ids: dict[tuple[str, str], str] = {}

    def get_room_meter(
        self,
        partment_id: str,
        floor_name: str,
        room_query: str,
    ) -> Optional[tuple[str, str, str]]:
        """Retrieve cached (drom_num, drom_name, floor_id) for a given room."""
        key = (partment_id, floor_name, str(room_query).strip())
        with self._lock:
            return self._room_meters.get(key)

    def set_room_meter(
        self,
        partment_id: str,
        floor_name: str,
        room_query: str,
        drom_num: str,
        drom_name: str,
        floor_id: str,
    ) -> None:
        """Cache room-to-meter mapping."""
        key = (partment_id, floor_name, str(room_query).strip())
        with self._lock:
            self._room_meters[key] = (drom_num, drom_name, floor_id)
            self._floor_ids[(partment_id, floor_name)] = floor_id

    def get_floor_id(self, partment_id: str, floor_name: str) -> Optional[str]:
        """Get cached floorId."""
        with self._lock:
            return self._floor_ids.get((partment_id, floor_name))

    def set_floor_id(self, partment_id: str, floor_name: str, floor_id: str) -> None:
        """Cache floorId."""
        with self._lock:
            self._floor_ids[(partment_id, floor_name)] = floor_id

    def clear(self) -> None:
        """Clear all cached metadata."""
        with self._lock:
            self._room_meters.clear()
            self._floor_ids.clear()


# Global default shared cache instance
GLOBAL_CACHE = MetadataCache()
