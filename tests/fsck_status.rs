//! Integration tests for the fsck part of the ODS status JSON.
//!
//! The omnect-os smoke tests read this shape (`ci/tests/destructive_test.sh`,
//! `ci/tests/factory_reset_test.sh`), so the key spelling and the
//! absent-when-clean behaviour are part of the contract, not an implementation
//! detail.

use omnect_os_init::OdsStatus;
use omnect_os_init::filesystem::FsckExitCode;
use omnect_os_init::partition::PartitionName;

#[test]
fn clean_run_omits_the_fsck_key() {
    let mut ods = OdsStatus::new();
    ods.record_fsck_result(PartitionName::Boot, FsckExitCode::OK, "clean".into());
    ods.record_fsck_result(PartitionName::Data, FsckExitCode::OK, "clean".into());

    let json = serde_json::to_value(&ods).unwrap();
    assert!(
        json.get("fsck").is_none(),
        "clean partitions must leave the fsck key out: {json}"
    );
}

#[test]
fn failing_partition_is_keyed_by_its_canonical_name() {
    let mut ods = OdsStatus::new();
    ods.record_fsck_result(
        PartitionName::Data,
        FsckExitCode::CORRECTED,
        "errors corrected on pass 1".into(),
    );

    let json = serde_json::to_value(&ods).unwrap();
    let entry = &json["fsck"]["data"];
    assert_eq!(entry["code"], 1, "code must be a bare integer: {json}");
    assert_eq!(entry["output"], "errors corrected on pass 1");
}

#[test]
fn only_failing_partitions_appear() {
    // What destructive_test.sh asserts: a clean boot partition reads as null
    // while the partitions it corrupted on purpose carry an entry.
    let mut ods = OdsStatus::new();
    ods.record_fsck_result(PartitionName::Boot, FsckExitCode::OK, "clean".into());
    ods.record_fsck_result(
        PartitionName::Cert,
        FsckExitCode::CORRECTED,
        "errors corrected".into(),
    );
    ods.record_fsck_result(
        PartitionName::Etc,
        FsckExitCode::CORRECTED,
        "errors corrected".into(),
    );
    ods.record_fsck_result(
        PartitionName::Data,
        FsckExitCode::ERRORS_UNCORRECTED,
        "uncorrected errors".into(),
    );

    let json = serde_json::to_value(&ods).unwrap();
    let fsck = json["fsck"].as_object().unwrap();
    assert!(!fsck.contains_key("boot"), "got: {json}");
    for name in ["cert", "etc", "data"] {
        assert!(fsck.contains_key(name), "missing {name}: {json}");
    }
}
