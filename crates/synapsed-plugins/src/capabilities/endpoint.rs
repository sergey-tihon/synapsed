use mlua::{MetaMethod, UserData, UserDataFields, UserDataMethods};
use synapsed_core::EndpointDescriptor;

#[derive(Clone)]
pub(crate) struct Endpoint {
    pub descriptor: EndpointDescriptor,
    pub owner: std::sync::Arc<str>,
}
impl UserData for Endpoint {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("capability", |_, this| {
            Ok(this.descriptor.capability.clone())
        });
        fields.add_field_method_get("manufacturer", |_, this| {
            Ok(this.descriptor.manufacturer.clone())
        });
        fields.add_field_method_get("product", |_, this| Ok(this.descriptor.product));
        fields.add_field_method_get("productName", |_, this| {
            Ok(this.descriptor.product_name.clone())
        });
        fields.add_field_method_get("identity", |_, this| Ok(this.descriptor.identity.clone()));
    }
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::ToString, |_, _, ()| Ok("endpoint"));
    }
}
