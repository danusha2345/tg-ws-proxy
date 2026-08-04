use jni::JNIEnv;
use jni::objects::{JClass, JString};
use jni::sys::jstring;

fn java_string(env: &JNIEnv<'_>, value: String) -> jstring {
    env.new_string(value)
        .map_or(std::ptr::null_mut(), JString::into_raw)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_danusha_tgwsproxy_NativeBridge_nativeStart(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    config: JString<'_>,
) -> jstring {
    let value = match env.get_string(&config) {
        Ok(config) => tg_ws_proxy_mobile::start(&String::from(config)),
        Err(error) => format!(r#"{{"ok":false,"error":"invalid configuration string: {error}"}}"#),
    };
    java_string(&env, value)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_danusha_tgwsproxy_NativeBridge_nativeStop(
    env: JNIEnv<'_>,
    _class: JClass<'_>,
) -> jstring {
    java_string(&env, tg_ws_proxy_mobile::stop())
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_danusha_tgwsproxy_NativeBridge_nativeStatus(
    env: JNIEnv<'_>,
    _class: JClass<'_>,
) -> jstring {
    java_string(&env, tg_ws_proxy_mobile::status())
}
