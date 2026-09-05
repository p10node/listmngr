use listmngr_core::{DeliveryMode, DeliveryStatus, Preferences};

fn expected<T: Clone>(values: &[Option<T>; 4]) -> Option<T> {
    values.iter().rev().find_map(Clone::clone)
}

fn assert_boolean_field(
    field: &str,
    set: fn(&mut Preferences, Option<bool>),
    get: fn(&Preferences) -> Option<bool>,
) {
    let choices = [None, Some(false), Some(true)];
    for system in choices {
        for user in choices {
            for address in choices {
                for member in choices {
                    let values = [system, user, address, member];
                    let mut layers: [Preferences; 4] =
                        std::array::from_fn(|_| Preferences::default());
                    for (layer, value) in layers.iter_mut().zip(values) {
                        set(layer, value);
                    }
                    let resolved = Preferences::resolve(layers.iter());
                    assert_eq!(
                        get(&resolved),
                        expected(&values),
                        "{field} failed for system/user/address/member={values:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn every_nullable_boolean_preference_obeys_all_four_layers() {
    assert_boolean_field(
        "acknowledge_posts",
        |p, value| p.acknowledge_posts = value,
        |p| p.acknowledge_posts,
    );
    assert_boolean_field(
        "hide_address",
        |p, value| p.hide_address = value,
        |p| p.hide_address,
    );
    assert_boolean_field(
        "receive_list_copy",
        |p, value| p.receive_list_copy = value,
        |p| p.receive_list_copy,
    );
    assert_boolean_field(
        "receive_own_postings",
        |p, value| p.receive_own_postings = value,
        |p| p.receive_own_postings,
    );
}

#[test]
fn differential_non_boolean_values_reject_constant_and_reversed_resolution() {
    let languages = ["system", "user", "address", "member"];
    let modes = [
        DeliveryMode::Regular,
        DeliveryMode::PlaintextDigests,
        DeliveryMode::MimeDigests,
        DeliveryMode::SummaryDigests,
    ];
    let statuses = [
        DeliveryStatus::Enabled,
        DeliveryStatus::ByUser,
        DeliveryStatus::ByBounces,
        DeliveryStatus::ByModerator,
    ];

    for mask in 0_u8..16 {
        let mut layers: [Preferences; 4] = std::array::from_fn(|_| Preferences::default());
        let mut language_values = std::array::from_fn(|_| None);
        let mut mode_values = [None; 4];
        let mut status_values = [None; 4];
        for layer in 0..4 {
            if mask & (1 << layer) != 0 {
                language_values[layer] = Some(languages[layer].to_owned());
                mode_values[layer] = Some(modes[layer]);
                status_values[layer] = Some(statuses[layer]);
            }
            layers[layer].preferred_language = language_values[layer].clone();
            layers[layer].delivery_mode = mode_values[layer];
            layers[layer].delivery_status = status_values[layer];
        }
        let resolved = Preferences::resolve(layers.iter());
        assert_eq!(
            resolved.preferred_language,
            expected(&language_values),
            "mask={mask:04b}"
        );
        assert_eq!(
            resolved.delivery_mode,
            expected(&mode_values),
            "mask={mask:04b}"
        );
        assert_eq!(
            resolved.delivery_status,
            expected(&status_values),
            "mask={mask:04b}"
        );
    }
}
