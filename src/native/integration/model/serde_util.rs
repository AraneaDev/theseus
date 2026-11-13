use serde::Deserialize;

pub(super) fn required_optional<'de, D, T>(decoder: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Option::<T>::deserialize(decoder)
}
