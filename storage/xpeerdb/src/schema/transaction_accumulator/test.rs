// Copyright (c) The XPeer Core Contributors
// SPDX-License-Identifier: Apache-2.0

use super::*;
use schemadb::schema::assert_encode_decode;

#[test]
fn test_encode_decode() {
    assert_encode_decode::<TransactionAccumulatorSchema>(
        &Position::from_inorder_index(100),
        &HashValue::random(),
    );
}
