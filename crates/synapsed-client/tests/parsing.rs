use synapsed_api::{
    Characteristic, CharacteristicAccess, CharacteristicCommand, CharacteristicSpec,
    CharacteristicStatus, CharacteristicValue, EnumOption,
};
use synapsed_client::parse_value;

fn characteristic(spec: CharacteristicSpec) -> Characteristic {
    Characteristic {
        id: "measurement".into(),
        label: "Measurement".into(),
        spec,
        access: CharacteristicAccess {
            read: false,
            write: true,
            notify: false,
        },
        value: None,
        status: CharacteristicStatus::Unavailable,
        error: None,
    }
}
#[test]
fn complete_finite_numbers_respect_fractional_and_integer_metadata() {
    let mut metadata = characteristic(CharacteristicSpec::Number {
        minimum: Some(-5.0),
        maximum: Some(10.0),
        step: None,
        integer: false,
        unit: Some("m".into()),
    });
    assert_eq!(
        parse_value(&metadata, "-1.25").unwrap(),
        CharacteristicValue::Number(-1.25)
    );
    assert_eq!(
        parse_value(&metadata, "1e1").unwrap(),
        CharacteristicValue::Number(10.0)
    );
    for argument in ["", "1.2junk", " 1", "1 ", "NaN", "inf", "1e999", "11"] {
        assert!(parse_value(&metadata, argument).is_err());
    }
    if let CharacteristicSpec::Number { integer, .. } = &mut metadata.spec {
        *integer = true;
    }
    assert!(parse_value(&metadata, "1.25").is_err());
    assert_eq!(
        parse_value(&metadata, "2.0").unwrap(),
        CharacteristicValue::Number(2.0)
    );
    assert!(
        metadata
            .validate_command(&CharacteristicCommand::Step { steps: 1 })
            .is_err()
    );
    assert!(
        metadata
            .validate_command(&CharacteristicCommand::Toggle {})
            .is_err()
    );
}
#[test]
fn booleans_enums_and_unicode_strings_have_exact_values() {
    let boolean = characteristic(CharacteristicSpec::Boolean);
    assert_eq!(
        parse_value(&boolean, "false").unwrap(),
        CharacteristicValue::Boolean(false)
    );
    for value in ["FALSE", "0", "true "] {
        assert!(parse_value(&boolean, value).is_err());
    }
    let enumeration = characteristic(CharacteristicSpec::Enum {
        options: vec![EnumOption {
            value: "low".into(),
            label: "Low".into(),
        }],
    });
    assert_eq!(
        parse_value(&enumeration, "low").unwrap(),
        CharacteristicValue::String("low".into())
    );
    assert!(parse_value(&enumeration, "Low").is_err());
    let string = characteristic(CharacteristicSpec::String { max_length: 2 });
    assert_eq!(
        parse_value(&string, "").unwrap(),
        CharacteristicValue::String(String::new())
    );
    assert_eq!(
        parse_value(&string, "🌙é").unwrap(),
        CharacteristicValue::String("🌙é".into())
    );
    assert!(parse_value(&string, "🌙éx").is_err());
    let action = characteristic(CharacteristicSpec::Action);
    assert!(parse_value(&action, "{}").is_err());
    assert!(
        action
            .validate_command(&CharacteristicCommand::Invoke {})
            .is_ok()
    );
    assert!(
        action
            .validate_command(&CharacteristicCommand::Set {
                value: CharacteristicValue::String(String::new())
            })
            .is_err()
    );
    let mut readonly = string;
    readonly.access.write = false;
    assert!(parse_value(&readonly, "").is_err());
}
