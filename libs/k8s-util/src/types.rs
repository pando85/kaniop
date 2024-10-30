use std::any::type_name;

#[inline]
pub fn short_type_name<K>() -> Option<&'static str> {
    let type_name = type_name::<K>();
    type_name.split("::").last()
}
