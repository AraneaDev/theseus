"""Exercise the test evidence boundary without requiring a Docker daemon."""
import subprocess
import sys
import unittest

sys.dont_write_bytecode = True
from docker_absence import require_container_absent


class DockerAbsenceTests(unittest.TestCase):
    def test_explicit_missing_target_is_accepted(self):
        for error in ["error: no such object: owned-id\n",
                      "Error: No such object: owned-id\n",
                      "Error response from daemon: No such container: owned-id\n"]:
            with self.subTest(error=error):
                require_container_absent("owned-id", subprocess.CompletedProcess(
                    ["docker", "inspect", "owned-id"], 1, "[]\n", error))

    def test_transport_or_unproven_absence_is_rejected(self):
        for status, error in [
            (1, "Cannot connect to the Docker daemon at unix:///missing.sock"),
            (1, "permission denied while trying to connect to the docker API"),
            (1, "error: no such object: another-id"),
            (1, "error: no such object: owned-id\ntransport disconnected"),
            (0, "error: no such object: owned-id"),
            (125, "error: no such object: owned-id"),
            (-9, "error: no such object: owned-id"),
            (1, ""),
        ]:
            with self.subTest(status=status, error=error):
                with self.assertRaises(AssertionError):
                    require_container_absent("owned-id", subprocess.CompletedProcess(
                        ["docker", "inspect", "owned-id"], status, "[]\n", error))


if __name__ == "__main__":
    unittest.main()
