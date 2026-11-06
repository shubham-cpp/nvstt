//! Used only by scripts/check_delivery.py in a private compositor.
use nvstt::delivery::{NativeFirstSink, TextSink};
use std::{env, fs, path::Path, time::Duration};

fn main() {
    assert_eq!(env::var("NVSTT_PRIVATE_DELIVERY_TEST").as_deref(), Ok("1"));
    let socket = env::var("LIBEI_SOCKET").expect("private EI socket");
    let root = env::var("XDG_RUNTIME_DIR").unwrap();
    assert!(Path::new(&socket).starts_with(&root));
    assert!(root.contains("nvstt-delivery-"));
    let text = fs::read_to_string(env::args().nth(1).expect("fixture path")).unwrap();
    if env::var_os("NVSTT_PROBE_LEGACY").is_some() {
        let client =
            eitype::EiType::connect_socket(Path::new(&socket), Default::default()).unwrap();
        client.type_text(&text).unwrap();
        println!("submitted {} characters", text.chars().count());
        std::thread::sleep(Duration::from_secs(4));
    } else {
        let mut sink = NativeFirstSink::new();
        let result = sink.send_final_text(&text).unwrap();
        assert!(
            matches!(result, nvstt::domain::DeliveryOutcome::Delivered { .. }),
            "{result:?}"
        );
        println!("submitted {} characters", text.chars().count());
        std::thread::sleep(Duration::from_secs(4));
    }
}
