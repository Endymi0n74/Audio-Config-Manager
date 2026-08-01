import sys
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch


sys.path.insert(0, str(Path(__file__).parent))
import audio_gui


class AudioConfigTests(unittest.TestCase):
    def test_schema_v2_is_accepted(self):
        audio_gui._validate_config({
            "schema": audio_gui.SCHEMA_NAME,
            "schemaVersion": 2,
            "global": {},
            "applications": [],
        })

    def test_old_schema_is_rejected(self):
        with self.assertRaises(audio_gui.AudioConfigError):
            audio_gui._validate_config({"schema": audio_gui.SCHEMA_NAME, "schemaVersion": 1})

    def test_device_matching_prefers_id_then_exact_name(self):
        devices = [
            SimpleNamespace(id="id-1", name="Casque"),
            SimpleNamespace(id="id-2", name="Haut-parleurs"),
        ]
        found, method = audio_gui._match_route_device(
            {"deviceId": "ID-2", "deviceName": "Casque"}, devices
        )
        self.assertEqual(found.id, "id-2")
        self.assertEqual(method, "identifiant")
        found, method = audio_gui._match_route_device(
            {"deviceId": "absent", "deviceName": "casque"}, devices
        )
        self.assertEqual(found.id, "id-1")
        self.assertEqual(method, "nom")

    def test_zero_volume_is_written_not_skipped(self):
        captured = []

        class Endpoint:
            def SetMasterVolumeLevelScalar(self, value, _context):
                captured.append(value)

        class Interface:
            def QueryInterface(self, _kind):
                return Endpoint()

        raw = SimpleNamespace(_dev=SimpleNamespace(Activate=lambda *_args: Interface()))
        with patch.object(audio_gui, "_raw_audio_device", return_value=raw):
            audio_gui._set_endpoint_volume(SimpleNamespace(id="x"), 0)
        self.assertEqual(captured, [0.0])

    def test_selective_restore_filters_categories(self):
        original = {
            "global": {
                "playbackDevices": [{"id": "out"}],
                "recordingDevices": [{"id": "in"}],
                "defaults": {"playback": {"id": "out"}},
            },
            "applications": [{"processName": "app.exe"}],
        }
        filtered = audio_gui._selected_config(original, {
            "defaults": True,
            "playbackVolumes": False,
            "recordingVolumes": True,
            "applications": False,
        })
        self.assertEqual(filtered["global"]["playbackDevices"], [])
        self.assertEqual(filtered["global"]["recordingDevices"], [{"id": "in"}])
        self.assertEqual(filtered["global"]["defaults"], {"playback": {"id": "out"}})
        self.assertEqual(filtered["applications"], [])
        self.assertEqual(original["applications"], [{"processName": "app.exe"}])


if __name__ == "__main__":
    unittest.main()
