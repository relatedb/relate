//! The payload module's own unit tests; the store-level suites live in
//! `native/tests/payload.rs`.

use std::sync::Arc;

use super::write::row_group_digests;
use arrow::array::Int64Builder;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use parquet::file::properties::WriterProperties;

/// Two row groups in one file, each digesting its own byte span: an edit
/// inside one flips exactly that group's digest.
#[test]
fn a_one_byte_edit_flips_only_its_row_group_digest() {
    let file = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
    let mut bytes = Vec::new();
    {
        let properties = WriterProperties::builder()
            .set_max_row_group_size(16)
            .build();
        let mut writer = ArrowWriter::try_new(&mut bytes, file, Some(properties)).unwrap();
        for group in 0..2 {
            let mut ids = Int64Builder::with_capacity(16);
            for row in 0..16 {
                ids.append_value(group * 16 + row);
            }
            let batch = RecordBatch::try_new(
                Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)])),
                vec![Arc::new(ids.finish())],
            )
            .unwrap();
            writer.write(&batch).unwrap();
        }
        writer.close().unwrap();
    }
    let digests = row_group_digests(&bytes::Bytes::from(bytes.clone())).unwrap();
    assert_eq!(digests.len(), 2, "{digests:?}");
    assert_ne!(digests[0].crc32, digests[1].crc32);
    // One byte inside the first group's span: only its digest moves.
    let mut edited = bytes;
    edited[digests[0].start as usize + digests[0].len as usize / 2] ^= 0x01;
    let after = row_group_digests(&bytes::Bytes::from(edited)).unwrap();
    assert_ne!(after[0].crc32, digests[0].crc32);
    assert_eq!(after[1].crc32, digests[1].crc32);
}
