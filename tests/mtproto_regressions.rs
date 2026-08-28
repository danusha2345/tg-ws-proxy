use proptest::prelude::*;
use tg_ws_proxy::crypto::apply;
use tg_ws_proxy::mtproto::{MessageSplitter, Transport, build_crypto_context, parse_client_init};

fn decode_array<const N: usize>(value: &str) -> [u8; N] {
    hex::decode(value)
        .expect("test vector must be valid hexadecimal")
        .try_into()
        .unwrap_or_else(|bytes: Vec<u8>| {
            panic!(
                "test vector has {} bytes instead of the expected {N}",
                bytes.len()
            )
        })
}

fn encrypt_for_splitter(relay_init: &[u8; 64], plain: &[u8]) -> Vec<u8> {
    let key: [u8; 32] = relay_init[8..40].try_into().expect("fixed test slice");
    let iv: [u8; 16] = relay_init[40..56].try_into().expect("fixed test slice");
    let mut encrypt = tg_ws_proxy::crypto::new_aes_ctr(&key, &iv);
    let mut discarded_handshake_keystream = [0_u8; 64];
    apply(&mut encrypt, &mut discarded_handshake_keystream);

    let mut encrypted = plain.to_vec();
    apply(&mut encrypt, &mut encrypted);
    encrypted
}

#[test]
fn public_mtproto_api_matches_python_golden_vectors() {
    let secret = decode_array::<16>("00112233445566778899aabbccddeeff");
    let client_init = decode_array::<64>(
        "010102034142434408090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f\
         202122232425262728292a2b2c2d2e2f30313233343536378f3d4055c174d035",
    );
    let relay_init = decode_array::<64>(
        "010102034142434408090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f\
         202122232425262728292a2b2c2d2e2f3031323334353637752b166de2ff69d9",
    );

    let parsed = parse_client_init(&client_init, &secret).expect("known client init must parse");
    assert_eq!(parsed.dc, 4);
    assert!(parsed.media);
    assert_eq!(parsed.transport, Transport::PaddedIntermediate);

    let mut context = build_crypto_context(&parsed.prekey_iv, &secret, &relay_init);

    let mut upload = hex::decode("884e6246ef8d821c09c17679acd4c613").unwrap();
    apply(&mut context.upstream.client_decrypt, &mut upload);
    assert_eq!(hex::encode(&upload), "112233445566778899aabbccddeeff00");
    apply(&mut context.upstream.telegram_encrypt, &mut upload);
    assert_eq!(hex::encode(upload), "238bf26c6d9712c9bb1b2b71c04c4d0b");

    let mut download = hex::decode("d0733ebc65669fc8e2bf64a739046a69").unwrap();
    apply(&mut context.downstream.telegram_decrypt, &mut download);
    assert_eq!(hex::encode(&download), "ffeeddccbbaa99887766554433221100");
    apply(&mut context.downstream.client_encrypt, &mut download);
    assert_eq!(hex::encode(download), "f70f3013b1d97504f27da6683e4ed144");
}

#[test]
fn intermediate_splitter_preserves_encrypted_packets_across_chunk_boundaries() {
    let relay_init = decode_array::<64>(
        "010102034142434408090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f\
         202122232425262728292a2b2c2d2e2f3031323334353637752b166de2ff69d9",
    );
    let first_plain = [4_u8, 0, 0, 0, 0xaa, 0xbb, 0xcc, 0xdd];
    let second_plain = [3_u8, 0, 0, 0, 0x11, 0x22, 0x33];
    let plain = [first_plain.as_slice(), second_plain.as_slice()].concat();
    let encrypted = encrypt_for_splitter(&relay_init, &plain);

    let mut splitter = MessageSplitter::new(&relay_init, Transport::Intermediate, 1024);
    assert!(
        splitter
            .split_reencrypted(&plain[..2], &encrypted[..2])
            .is_empty()
    );
    assert!(
        splitter
            .split_reencrypted(&plain[2..7], &encrypted[2..7])
            .is_empty()
    );

    let first = splitter.split_reencrypted(&plain[7..10], &encrypted[7..10]);
    assert_eq!(first, [encrypted[..first_plain.len()].to_vec()]);

    let second = splitter.split_reencrypted(&plain[10..], &encrypted[10..]);
    assert_eq!(second, [encrypted[first_plain.len()..].to_vec()]);
    assert!(splitter.flush().is_empty());
}

#[test]
fn abridged_splitter_supports_extended_length_header() {
    let relay_init = decode_array::<64>(
        "010102034142434408090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f\
         202122232425262728292a2b2c2d2e2f3031323334353637752b166de2ff69d9",
    );
    let payload_words = 128_u32;
    let length = payload_words.to_le_bytes();
    let mut plain = vec![0x7f, length[0], length[1], length[2]];
    plain.extend((0_u8..=u8::MAX).cycle().take(512));
    let encrypted = encrypt_for_splitter(&relay_init, &plain);

    let mut splitter = MessageSplitter::new(&relay_init, Transport::Abridged, 1024);
    assert!(splitter.split(&encrypted[..3]).is_empty());
    assert!(splitter.split(&encrypted[3..400]).is_empty());
    assert_eq!(splitter.split(&encrypted[400..]), [encrypted]);
    assert!(splitter.flush().is_empty());
}

#[test]
fn oversized_declared_packet_disables_splitting_without_retaining_data() {
    let relay_init = decode_array::<64>(
        "010102034142434408090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f\
         202122232425262728292a2b2c2d2e2f3031323334353637752b166de2ff69d9",
    );
    let malicious_header = 1_000_000_u32.to_le_bytes();
    let encrypted_header = encrypt_for_splitter(&relay_init, &malicious_header);

    let mut splitter = MessageSplitter::new(&relay_init, Transport::Intermediate, 256);
    assert_eq!(splitter.split(&encrypted_header), [encrypted_header]);
    assert!(splitter.flush().is_empty());

    let following = vec![0x5a; 32];
    assert_eq!(splitter.split(&following), [following]);
    assert!(splitter.flush().is_empty());
}

#[test]
fn combined_chunk_over_buffer_cap_is_forwarded_once_without_retention() {
    let relay_init = decode_array::<64>(
        "010102034142434408090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f\
         202122232425262728292a2b2c2d2e2f3031323334353637752b166de2ff69d9",
    );
    let mut first_packet = 32_u32.to_le_bytes().to_vec();
    first_packet.extend([0x42; 32]);
    let mut second_packet = 32_u32.to_le_bytes().to_vec();
    second_packet.extend([0x24; 32]);
    let plain = [first_packet, second_packet].concat();
    let encrypted = encrypt_for_splitter(&relay_init, &plain);

    let mut splitter = MessageSplitter::new(&relay_init, Transport::Intermediate, 64);
    assert_eq!(splitter.split(&encrypted), [encrypted]);
    assert!(splitter.flush().is_empty());
}

#[test]
fn zero_length_abridged_packet_disables_splitting_without_frame_storm() {
    let relay_init = decode_array::<64>(
        "010102034142434408090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f\
         202122232425262728292a2b2c2d2e2f3031323334353637752b166de2ff69d9",
    );
    for plain in [
        vec![0, 0x11, 0x22, 0x33, 0x44],
        vec![0x80, 0x11, 0x22, 0x33, 0x44],
        vec![0x7f, 0, 0, 0, 0x11, 0x22, 0x33, 0x44],
        vec![0xff, 0, 0, 0, 0x11, 0x22, 0x33, 0x44],
    ] {
        let encrypted = encrypt_for_splitter(&relay_init, &plain);
        let mut splitter = MessageSplitter::new(&relay_init, Transport::Abridged, 1024);

        assert_eq!(splitter.split(&encrypted), [encrypted]);
        assert_eq!(splitter.split(&[0x55; 64]), [vec![0x55; 64]]);
        assert!(splitter.flush().is_empty());
    }
}

#[test]
fn zero_length_intermediate_packet_disables_splitting_without_frame_storm() {
    let relay_init = decode_array::<64>(
        "010102034142434408090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f\
         202122232425262728292a2b2c2d2e2f3031323334353637752b166de2ff69d9",
    );
    let encrypted = encrypt_for_splitter(&relay_init, &[0, 0, 0, 0, 0x11, 0x22, 0x33, 0x44]);
    let mut splitter = MessageSplitter::new(&relay_init, Transport::Intermediate, 1024);

    assert_eq!(splitter.split(&encrypted), [encrypted]);
    assert_eq!(splitter.split(&[0x55; 64]), [vec![0x55; 64]]);
    assert!(splitter.flush().is_empty());
}

proptest! {
    #[test]
    fn intermediate_splitter_is_independent_of_tcp_chunking(
        payloads in prop::collection::vec(prop::collection::vec(any::<u8>(), 1..512), 1..24),
        chunk_sizes in prop::collection::vec(1_usize..257, 1..64),
    ) {
        let relay_init = decode_array::<64>(
            "010102034142434408090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f\
             202122232425262728292a2b2c2d2e2f3031323334353637752b166de2ff69d9",
        );
        let packets = payloads
            .iter()
            .map(|payload| {
                let mut packet = u32::try_from(payload.len())
                    .expect("generated payload fits in u32")
                    .to_le_bytes()
                    .to_vec();
                packet.extend_from_slice(payload);
                packet
            })
            .collect::<Vec<_>>();
        let plain = packets.concat();
        let encrypted = encrypt_for_splitter(&relay_init, &plain);
        let mut expected = Vec::with_capacity(packets.len());
        let mut offset = 0;
        for packet in packets {
            expected.push(encrypted[offset..offset + packet.len()].to_vec());
            offset += packet.len();
        }

        let mut splitter = MessageSplitter::new(
            &relay_init,
            Transport::Intermediate,
            encrypted.len().saturating_add(1),
        );
        let mut actual = Vec::new();
        let mut offset = 0;
        for chunk_size in chunk_sizes.iter().cycle() {
            if offset == encrypted.len() {
                break;
            }
            let end = offset.saturating_add(*chunk_size).min(encrypted.len());
            actual.extend(splitter.split_reencrypted(
                &plain[offset..end],
                &encrypted[offset..end],
            ));
            offset = end;
        }

        prop_assert_eq!(actual, expected);
        prop_assert!(splitter.flush().is_empty());
    }
}
