//! 本机 WebSocket 统一来源、首帧身份与版本核验。
use super::{HttpState, auth, error::HttpError};
use crate::access::AccessPermit;
use axum::{
    extract::ConnectInfo,
    http::{HeaderMap, Request, header::AUTHORIZATION},
};
use std::net::SocketAddr;

pub(crate) struct SocketAuthentication {
    pub(crate) state: HttpState,
    pub(crate) headers: HeaderMap,
    pub(crate) peer: SocketAddr,
    pub(crate) permit: Option<AccessPermit>,
    pub(crate) target: Option<String>,
}

fn authenticate_http(
    auth: &SocketAuthentication,
    bearer: Option<&str>,
) -> Result<AccessPermit, HttpError> {
    if let Some(bearer) = bearer {
        let mut request = Request::new(axum::body::Body::empty());
        *request.headers_mut() = auth.headers.clone();
        request.headers_mut().insert(
            AUTHORIZATION,
            format!("Bearer {bearer}")
                .parse()
                .map_err(|_| HttpError::unauthorized())?,
        );
        request.extensions_mut().insert(ConnectInfo(auth.peer));
        let permit = auth::authorize_request(&auth.state, &request)?
            .1
            .ok_or_else(HttpError::unauthorized)?;
        if let Some(initial) = &auth.permit {
            initial.check().map_err(|_| HttpError::unauthorized())?;
            if initial.native != permit.native || initial.login_id() != permit.login_id() {
                return Err(HttpError::unauthorized());
            }
        }
        Ok(permit)
    } else {
        auth.permit
            .clone()
            .filter(|permit| permit.check().is_ok())
            .ok_or_else(HttpError::unauthorized)
    }
}

pub(crate) async fn authenticate(
    auth: &SocketAuthentication,
    bearer: Option<&str>,
    login_context: Option<&str>,
) -> Result<AccessPermit, crate::access::AccessError> {
    let fail = || crate::access::AccessError::Unauthorized;
    let mut permit = authenticate_http(auth, bearer).map_err(|_| fail())?;
    if bearer.is_none() && !permit.native && login_context != permit.login_context().as_deref() {
        return Err(fail());
    }
    if auth.state.access.center().is_some() {
        if permit.native {
            return Err(fail());
        }
        auth.state
            .access
            .credentials
            .verify_enterprise(&mut permit)
            .await
            .map_err(|_| fail())?;
    }
    Ok(permit)
}

pub(crate) fn validate_compatibility(
    headers: &HeaderMap,
    client: Option<&assistant_protocol::ClientCompatibility>,
) -> Result<(), assistant_protocol::RuntimeCompatibilityError> {
    use assistant_protocol::{
        ClientCompatibility, RuntimeCompatibilityError, RuntimeCompatibilityErrorCode,
        check_compatibility,
    };
    let host = ClientCompatibility::current();
    let declared = super::compatibility::declaration(headers)?;
    if let Some(header) = &declared {
        check_compatibility(Some(header), &host)?;
    }
    check_compatibility(client, &host)?;
    if declared
        .as_ref()
        .is_some_and(|header| Some(header) != client)
    {
        return Err(RuntimeCompatibilityError {
            code: RuntimeCompatibilityErrorCode::InvalidDeclaration,
            client: client.cloned(),
            host: Some(host),
        });
    }
    Ok(())
}

#[cfg(test)]
mod compatibility_tests {
    use super::validate_compatibility;
    use assistant_protocol::{ClientCompatibility, RuntimeCompatibilityErrorCode as Code};
    use axum::http::HeaderMap;

    #[test]
    fn frame_declaration_is_required_even_with_valid_headers() {
        let own = ClientCompatibility::current();
        let mut headers = HeaderMap::new();
        headers.insert(
            assistant_protocol::CLIENT_VERSION_HEADER,
            own.version.parse().unwrap(),
        );
        headers.insert(
            assistant_protocol::MIN_COMPATIBLE_VERSION_HEADER,
            own.min_compatible_version.parse().unwrap(),
        );
        assert_eq!(
            validate_compatibility(&headers, None).unwrap_err().code,
            Code::MissingDeclaration
        );
        assert!(validate_compatibility(&headers, Some(&own)).is_ok());
        let newer = ClientCompatibility {
            version: "99.0.0".into(),
            ..own
        };
        assert_eq!(
            validate_compatibility(&headers, Some(&newer))
                .unwrap_err()
                .code,
            Code::InvalidDeclaration
        );
        assert!(validate_compatibility(&HeaderMap::new(), Some(&newer)).is_ok());
    }

    #[test]
    fn incompatible_or_partial_headers_cannot_be_hidden_by_the_frame() {
        let own = ClientCompatibility::current();
        let mut headers = HeaderMap::new();
        headers.insert(
            assistant_protocol::CLIENT_VERSION_HEADER,
            "0.25.1".parse().unwrap(),
        );
        assert_eq!(
            validate_compatibility(&headers, Some(&own))
                .unwrap_err()
                .code,
            Code::InvalidDeclaration
        );
        headers.insert(
            assistant_protocol::MIN_COMPATIBLE_VERSION_HEADER,
            "0.25.1".parse().unwrap(),
        );
        assert_eq!(
            validate_compatibility(&headers, Some(&own))
                .unwrap_err()
                .code,
            Code::ClientTooOld
        );
        let newer_floor = ClientCompatibility {
            version: "99.0.0".into(),
            min_compatible_version: "99.0.0".into(),
        };
        assert_eq!(
            validate_compatibility(&HeaderMap::new(), Some(&newer_floor))
                .unwrap_err()
                .code,
            Code::HostTooOld
        );
    }
}
