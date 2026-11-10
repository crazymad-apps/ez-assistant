//! 选组织前只保存短期选择，不能用于访问工作区或装配 Runtime。

use std::{sync::Arc, time::Duration};

use assistant_protocol::{
    ClientCompatibility, HostOrganization, HostOrganizationSelection, SecretValue,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{CenterClient, CenterError, CenterLogin, Credentials, UserKey, client::Login};
use crate::access::{
    AccessError, AccessPermit, HostAccessHandle,
    credentials::{LoginSession, digest},
};

pub(in crate::access) struct OrganizationSelection {
    login: Login,
    client: CenterClient,
    expires: Instant,
    pub(super) native: bool,
}

impl Credentials {
    pub(crate) fn invalidate_switch_login(&self, key: &UserKey, login: &Arc<CenterLogin>) {
        let mut state = self.state.lock().expect("credential state poisoned");
        if state
            .users
            .get(key)
            .is_some_and(|user| Arc::ptr_eq(&user.login, login))
        {
            super::end_user(&mut state, key);
        }
    }

    pub(crate) async fn switch_organizations(
        &self,
        permit: &AccessPermit,
    ) -> Result<(Arc<CenterLogin>, Vec<HostOrganization>), AccessError> {
        permit.check()?;
        let key = permit.user_key().ok_or(AccessError::Unauthorized)?;
        let (login, _) = self.current_center_login(key)?;
        let organizations = login.client.organizations(&login.token).await?;
        Ok((
            login,
            organizations
                .into_iter()
                .filter(|org| org.id != key.organization_id)
                .collect(),
        ))
    }

    pub(crate) async fn exchange_organization(
        &self,
        access: &HostAccessHandle,
        permit: &AccessPermit,
        source: Arc<CenterLogin>,
        organization_id: &str,
        listener: &CancellationToken,
    ) -> Result<(SecretValue, LoginSession), AccessError> {
        permit.check()?;
        let key = permit.user_key().ok_or(AccessError::Unauthorized)?;
        let compatibility = permit
            .compatibility()
            .ok_or(AccessError::Unauthorized)?
            .clone();
        let (current, _) = self.current_center_login(key)?;
        if !Arc::ptr_eq(&current, &source) {
            return Err(CenterError::StateChanged.into());
        }
        let login = source.client.select(&source.token, organization_id).await?;
        let identity = &login.identity;
        if identity.center_id != key.center_id
            || identity.user.id != key.user_id
            || identity.legacy_organization_id != source.legacy_organization_id
            || identity.scope.organization_id.as_deref() != Some(organization_id)
        {
            let _ = source.client.logout(&login.token).await;
            return Err(CenterError::IdentityMismatch.into());
        }
        self.finish_enterprise_login(
            &source.client,
            access,
            login,
            compatibility,
            listener,
            Some((key, &source)),
        )
        .await
        .map_err(|_| CenterError::IdentityMismatch.into())
    }

    pub(crate) async fn begin_enterprise_login(
        &self,
        client: &CenterClient,
        username: String,
        password: SecretValue,
        native: bool,
    ) -> Result<HostOrganizationSelection, AccessError> {
        let login = client.login(username, password).await?;
        let result = async {
            let organizations = client.organizations(&login.token).await?;
            let mut bytes = [0; 32];
            getrandom::fill(&mut bytes).map_err(|_| AccessError::Unavailable)?;
            let token = URL_SAFE_NO_PAD.encode(bytes);
            let mut state = self.state.lock().expect("credential state poisoned");
            let now = Instant::now();
            state
                .organization_selections
                .retain(|_, selection| selection.expires > now);
            if state.organization_selections.len() >= 64 {
                return Err(AccessError::Busy);
            }
            state.organization_selections.insert(
                digest(&token),
                OrganizationSelection {
                    login: Login {
                        identity: login.identity.clone(),
                        token: login.token.clone(),
                        llm_key: None,
                    },
                    client: client.clone(),
                    expires: now + Duration::from_secs(300),
                    native,
                },
            );
            Ok(HostOrganizationSelection {
                selection_token: SecretValue::new(token),
                organizations,
            })
        }
        .await;
        if result.is_err() {
            let _ = client.logout(&login.token).await;
        }
        result
    }

    pub(crate) fn selection_native(&self, token: &SecretValue) -> Result<bool, AccessError> {
        let state = self.state.lock().expect("credential state poisoned");
        state
            .organization_selections
            .get(&digest(token.expose()))
            .filter(|selection| selection.expires > Instant::now())
            .map(|selection| selection.native)
            .ok_or(AccessError::Unauthorized)
    }

    pub(crate) async fn select_enterprise_organization(
        &self,
        access: &HostAccessHandle,
        token: SecretValue,
        organization_id: String,
        compatibility: ClientCompatibility,
        listener: &CancellationToken,
    ) -> Result<(SecretValue, LoginSession, bool), AccessError> {
        if !super::client::valid_center_id(&organization_id) {
            return Err(CenterError::InvalidRequest.into());
        }
        let pending = self
            .state
            .lock()
            .expect("credential state poisoned")
            .organization_selections
            .remove(&digest(token.expose()))
            .filter(|selection| selection.expires > Instant::now())
            .ok_or(AccessError::Unauthorized)?;
        let login = match pending
            .client
            .select(&pending.login.token, &organization_id)
            .await
        {
            Ok(login) => login,
            Err(error) => {
                let _ = pending.client.logout(&pending.login.token).await;
                return Err(error.into());
            }
        };
        let identity = &login.identity;
        let original = &pending.login.identity;
        if identity.center_id != original.center_id
            || identity.user.id != original.user.id
            || identity.legacy_organization_id != original.legacy_organization_id
            || identity.scope.organization_id.as_deref() != Some(organization_id.as_str())
        {
            let _ = pending.client.logout(&login.token).await;
            return Err(CenterError::IdentityMismatch.into());
        }
        self.finish_enterprise_login(
            &pending.client,
            access,
            login,
            compatibility,
            listener,
            None,
        )
        .await
        .map(|(token, session)| (token, session, pending.native))
    }
}
