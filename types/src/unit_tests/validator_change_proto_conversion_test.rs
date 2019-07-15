// Copyright (c) The XPeer Core Contributors
// SPDX-License-Identifier: Apache-2.0

use crate::validator_change::ValidatorChangeEventWithProof;
use proptest::prelude::*;
use proto_conv::test_helper::assert_protobuf_encode_decode;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(20))]

    #[test]
    fn test_validator_change_event_with_proof_conversion(
        change in any::<ValidatorChangeEventWithProof>()
    ) {
        assert_protobuf_encode_decode(&change);
    }
}
