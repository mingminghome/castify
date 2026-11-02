use crate::xml::escape;

#[derive(Debug, Clone)]
pub struct DeviceIdentity {
    pub friendly_name: String,
    pub uuid: String,
    pub manufacturer: String,
    pub model_name: String,
    pub model_number: String,
}

impl DeviceIdentity {
    pub fn new(friendly_name: impl Into<String>, uuid: impl Into<String>) -> Self {
        Self {
            friendly_name: one_line(&friendly_name.into()),
            uuid: one_line(&strip_uuid_prefix(&uuid.into())),
            manufacturer: "castify".into(),
            model_name: "castify".into(),
            model_number: "1".into(),
        }
    }

    pub fn usn_root(&self) -> String {
        format!("uuid:{}", self.uuid)
    }

    pub fn usn(&self, st: &str) -> String {
        let root = self.usn_root();
        if st.eq_ignore_ascii_case(&root) {
            root
        } else {
            format!("{root}::{st}")
        }
    }
}

fn strip_uuid_prefix(uuid: &str) -> String {
    uuid.trim()
        .trim_start_matches("uuid:")
        .trim()
        .to_string()
}

fn one_line(value: &str) -> String {
    value
        .chars()
        .filter(|c| *c != '\r' && *c != '\n' && *c != '\0')
        .collect()
}

pub fn device_description(id: &DeviceIdentity, base: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
  <specVersion><major>1</major><minor>0</minor></specVersion>
  <device>
    <deviceType>urn:schemas-upnp-org:device:MediaRenderer:1</deviceType>
    <friendlyName>{name}</friendlyName>
    <manufacturer>{mfr}</manufacturer>
    <manufacturerURL>https://github.com/mingminghome/castify</manufacturerURL>
    <modelDescription>Receives photo, audio, video, and web URLs on the local network</modelDescription>
    <modelName>{model}</modelName>
    <modelNumber>{number}</modelNumber>
    <UDN>uuid:{uuid}</UDN>
    <serviceList>
      <service>
        <serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType>
        <serviceId>urn:upnp-org:serviceId:AVTransport</serviceId>
        <SCPDURL>/avtransport.xml</SCPDURL>
        <controlURL>/avtransport/control</controlURL>
        <eventSubURL>/avtransport/event</eventSubURL>
      </service>
      <service>
        <serviceType>urn:schemas-upnp-org:service:RenderingControl:1</serviceType>
        <serviceId>urn:upnp-org:serviceId:RenderingControl</serviceId>
        <SCPDURL>/renderingcontrol.xml</SCPDURL>
        <controlURL>/renderingcontrol/control</controlURL>
        <eventSubURL>/renderingcontrol/event</eventSubURL>
      </service>
      <service>
        <serviceType>urn:schemas-upnp-org:service:ConnectionManager:1</serviceType>
        <serviceId>urn:upnp-org:serviceId:ConnectionManager</serviceId>
        <SCPDURL>/connectionmanager.xml</SCPDURL>
        <controlURL>/connectionmanager/control</controlURL>
        <eventSubURL>/connectionmanager/event</eventSubURL>
      </service>
    </serviceList>
    <presentationURL>{base}/</presentationURL>
  </device>
</root>
"#,
        name = escape(&id.friendly_name),
        mfr = escape(&id.manufacturer),
        model = escape(&id.model_name),
        number = escape(&id.model_number),
        uuid = escape(&id.uuid),
        base = escape(base)
    )
}

pub fn av_transport_scpd() -> &'static str {
    r#"<?xml version="1.0" encoding="utf-8"?>
<scpd xmlns="urn:schemas-upnp-org:service-1-0">
  <specVersion><major>1</major><minor>0</minor></specVersion>
  <actionList>
    <action><name>SetAVTransportURI</name></action>
    <action><name>Play</name></action>
    <action><name>Pause</name></action>
    <action><name>Stop</name></action>
    <action><name>Seek</name></action>
    <action><name>GetTransportInfo</name></action>
    <action><name>GetMediaInfo</name></action>
    <action><name>GetPositionInfo</name></action>
    <action><name>GetDeviceCapabilities</name></action>
    <action><name>GetTransportSettings</name></action>
    <action><name>GetCurrentTransportActions</name></action>
  </actionList>
  <serviceStateTable>
    <stateVariable sendEvents="no"><name>TransportState</name><dataType>string</dataType></stateVariable>
    <stateVariable sendEvents="no"><name>AVTransportURI</name><dataType>string</dataType></stateVariable>
  </serviceStateTable>
</scpd>
"#
}

pub fn rendering_control_scpd() -> &'static str {
    r#"<?xml version="1.0" encoding="utf-8"?>
<scpd xmlns="urn:schemas-upnp-org:service-1-0">
  <specVersion><major>1</major><minor>0</minor></specVersion>
  <actionList>
    <action><name>GetVolume</name></action>
    <action><name>SetVolume</name></action>
    <action><name>GetMute</name></action>
    <action><name>SetMute</name></action>
  </actionList>
  <serviceStateTable>
    <stateVariable sendEvents="no"><name>Volume</name><dataType>ui2</dataType></stateVariable>
    <stateVariable sendEvents="no"><name>Mute</name><dataType>boolean</dataType></stateVariable>
  </serviceStateTable>
</scpd>
"#
}

pub fn connection_manager_scpd() -> &'static str {
    r#"<?xml version="1.0" encoding="utf-8"?>
<scpd xmlns="urn:schemas-upnp-org:service-1-0">
  <specVersion><major>1</major><minor>0</minor></specVersion>
  <actionList>
    <action><name>GetProtocolInfo</name></action>
    <action><name>GetCurrentConnectionIDs</name></action>
    <action><name>GetCurrentConnectionInfo</name></action>
  </actionList>
  <serviceStateTable>
    <stateVariable sendEvents="no"><name>SinkProtocolInfo</name><dataType>string</dataType></stateVariable>
  </serviceStateTable>
</scpd>
"#
}

pub const SINK_PROTOCOL_INFO: &str = concat!(
    "http-get:*:image/jpeg:*,",
    "http-get:*:image/png:*,",
    "http-get:*:image/gif:*,",
    "http-get:*:image/webp:*,",
    "http-get:*:image/bmp:*,",
    "http-get:*:video/mp4:*,",
    "http-get:*:video/mpeg:*,",
    "http-get:*:video/quicktime:*,",
    "http-get:*:audio/mpeg:*,",
    "http-get:*:audio/mp4:*,",
    "http-get:*:application/x-mpegurl:*,",
    "http-get:*:application/vnd.apple.mpegurl:*"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn description_mentions_renderer() {
        let id = DeviceIdentity::new("TV,", "abc-def");
        let xml = device_description(&id, "http://10.0.0.5:2870");
        assert!(xml.contains("MediaRenderer:1"));
        assert!(xml.contains("uuid:abc-def"));
        assert!(xml.contains("TV,"));
    }

    #[test]
    fn description_escapes_name_and_flattens_uuid() {
        let id = DeviceIdentity::new("A<B&C\n", "u\nu\0");
        assert!(!id.friendly_name.contains('\n'));
        assert!(!id.uuid.contains('\n'));
        assert!(!id.uuid.contains('\0'));
        let xml = device_description(&id, "http://10.0.0.5:1");
        assert!(xml.contains("<friendlyName>A&lt;B&amp;C</friendlyName>"));
        assert!(xml.contains("<UDN>uuid:uu</UDN>"));
    }

    #[test]
    fn usn_for_uuid_is_bare() {
        let id = DeviceIdentity::new("TV,", "abc-def");
        assert_eq!(id.usn("uuid:abc-def"), "uuid:abc-def");
        assert_eq!(
            id.usn("upnp:rootdevice"),
            "uuid:abc-def::upnp:rootdevice"
        );
    }
}
