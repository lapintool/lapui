"""Failure-path checks for private-file exclusion and source/archive integrity."""
import hashlib
import io
from pathlib import Path
import tarfile
import tempfile
import unittest
import zipfile

import package_preview as package
import verify_preview as verify


class PreviewGuards(unittest.TestCase):
    def test_plans_credentials_and_build_outputs_never_enter_project_sources(self):
        for relative in ("docs/NEXT_PLAN.md", "Docs/plan.md", ".ssh/id_ed25519", "target/lapui.exe", "examples/demo/node_modules/pkg/index.js", ".env", "key.pem"):
            with self.subTest(relative=relative), self.assertRaises(ValueError):
                package.safe_source(package.ROOT / relative)
        self.assertEqual(package.safe_source(package.ROOT / "src/runtime.rs"), "src/runtime.rs")

    def test_source_collector_excludes_dev_only_edges_and_keeps_build_transitives(self):
        def edge(name, kind):
            return {"pkg":name, "dep_kinds":[{"kind":kind}]}
        data = {"packages":[{"id":name,"name":name,"version":"1","source":"registry+test"} for name in ("root","normal","build","dev","nested")],
                "resolve":{"root":"root", "nodes":[
                    {"id":"root","deps":[edge("normal",None),edge("build","build"),edge("dev","dev")]},
                    {"id":"normal","deps":[]}, {"id":"build","deps":[edge("nested",None)]},
                    {"id":"dev","deps":[]}, {"id":"nested","deps":[]}]}}
        self.assertEqual([item["name"] for item in package.registry_packages(data)], ["build","nested","normal"])

    def test_registry_source_drift_is_rejected_even_with_a_valid_download(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            local = root / "src/index/sample-1.0.0"
            local.mkdir(parents=True)
            (local / "lib.rs").write_bytes(b"original source")
            archive = root / "cache/index/sample-1.0.0.crate"
            archive.parent.mkdir(parents=True)
            with tarfile.open(archive, "w:gz") as output:
                entry = tarfile.TarInfo("sample-1.0.0/lib.rs")
                entry.size = len(b"original source")
                output.addfile(entry, io.BytesIO(b"original source"))
            item = {"name":"sample", "version":"1.0.0", "manifest_path":str(local / "Cargo.toml")}
            checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
            self.assertEqual(package.registry_archive(item, checksum)[0], archive)
            (local / "lib.rs").write_bytes(b"unrecorded patch")
            with self.assertRaisesRegex(ValueError, "modified registry source"):
                package.registry_archive(item, checksum)

    def test_zip_traversal_and_duplicate_paths_are_rejected_before_extraction(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive = root / "bad.zip"
            destination = root / "extracted"
            destination.mkdir()
            with zipfile.ZipFile(archive, "w") as output:
                output.writestr("../escape.txt", "must not be written")
            with self.assertRaises(ValueError):
                verify.extract(archive, destination)
            self.assertFalse((root / "escape.txt").exists())
            with zipfile.ZipFile(archive, "w") as output:
                output.writestr("app/Data.txt", "one")
                output.writestr("app/data.txt", "two")
            with self.assertRaisesRegex(ValueError, "duplicate"):
                verify.extract(archive, destination)
            self.assertEqual(list(destination.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
