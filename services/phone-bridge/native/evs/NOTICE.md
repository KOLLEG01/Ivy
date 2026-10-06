# EVS reference dependency

`ivy_phone_evs.dll` is built from the separately obtained 3GPP EVS floating-point reference
implementation distributed by ETSI with TS 126 443 V19.0.0. Its nested source archive is
`26443-i00-ANSI-C_source_code.zip`; the source readme identifies version 16.3.0.

Source: https://www.etsi.org/deliver/etsi_ts/126400_126499/126443/19.00.00_60/ts_126443v190000p0.zip

Archive SHA-256: `f3e0928f917fe67a620a37578757b0db00e0ca216481a96a99ea8ff5e3b8271b`.

The Ivy adapter adds the streaming ABI and compiles the reference library without its
standalone encoder/decoder programs. `evs-build.json` records the authored input hashes,
reference, compiler and exact library hash for this package. The repository does not
redistribute the reference source. This provenance notice does not grant additional
rights to the reference implementation or the EVS technology.
