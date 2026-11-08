# Patched dependencies

Copies of two IronRDP 0.9.0 crates (MIT or Apache-2.0, licenses included), used through
`[patch.crates-io]` in the root `Cargo.toml`. Drop each patch once upstream has an equivalent.

- `ironrdp-rdpdr`: `DeviceAnnounceHeader::new_drive` (`src/pdu/efs.rs`) sends the shared
  folder's name as ANSI, as FreeRDP does. With the Unicode name, Windows 10 hosts showed only the
  first letter ("m on HOST") and could not open the drive.
- `ironrdp-rdpdr-native`: `redirected_root_nt_path` (`src/windows/handles.rs`) opens the folder
  through its volume device (`\Device\HarddiskVolumeN\...`, from `QueryDosDeviceW`) instead of
  `\??\C:\...`. On Windows 10 the `\??\C:` link counted as a reparse point under
  `OBJ_DONT_REPARSE`, so the root open failed with STATUS_REPARSE_POINT_ENCOUNTERED.
