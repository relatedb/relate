use std::cell::RefCell;

use super::*;

#[test]
fn staged_rows_install_a_slice_each_time_they_reach_the_bound() {
    // A 100-byte slice over rows of 30 bytes: every fourth row installs
    // the four staged, and the flush installs the remainder once.
    let slices = RefCell::new(Vec::new());
    let mut staged = Staged::new(100, |rows: &[i64]| {
        slices.borrow_mut().push(rows.to_vec());
        Ok(())
    });
    for row in 0..10 {
        staged.push(row, 30).unwrap();
    }
    staged.flush().unwrap();
    staged.flush().unwrap();
    assert_eq!(
        slices.into_inner(),
        vec![vec![0, 1, 2, 3], vec![4, 5, 6, 7], vec![8, 9]]
    );
}

#[test]
fn staged_rows_install_at_the_codec_row_ceiling_under_any_byte_bound() {
    // Rows too small to reach the byte bound still install a row ceiling
    // at a time.
    let sizes = RefCell::new(Vec::new());
    let mut staged = Staged::new(u64::MAX, |rows: &[usize]| {
        sizes.borrow_mut().push(rows.len());
        Ok(())
    });
    for row in 0..INSTALL_SLICE_ROWS + 5 {
        staged.push(row, 1).unwrap();
    }
    staged.flush().unwrap();
    assert_eq!(sizes.into_inner(), vec![INSTALL_SLICE_ROWS, 5]);
}
