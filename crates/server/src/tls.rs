use axum_server::tls_rustls::RustlsConfig;
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};
use std::net::IpAddr;

pub async fn generate_self_signed_config(lan_ip: Option<IpAddr>) -> Result<RustlsConfig, Box<dyn std::error::Error>> {
    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "dchat-local");
    dn.push(DnType::OrganizationName, "Ephemeral P2P Chat");
    params.distinguished_name = dn;

    params.subject_alt_names.push(SanType::DnsName("localhost".try_into()?));
    params.subject_alt_names.push(SanType::IpAddress(IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1))));

    if let Some(ip) = lan_ip {
        params.subject_alt_names.push(SanType::IpAddress(ip));
    }

    let key_pair = KeyPair::generate()?;
    let cert = params.self_signed(&key_pair)?;

    let cert_pem = cert.pem();
    let key_pem = key_pair.serialize_pem();

    let config = RustlsConfig::from_pem(cert_pem.into_bytes(), key_pem.into_bytes()).await?;
    Ok(config)
}
