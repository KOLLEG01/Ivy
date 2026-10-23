use super::*;
fn field<'a>(v: &'a IvyJson, k: &str) -> IvyResult<&'a IvyJson> {
    v.get(k).ok_or_else(IvyError::internal)
}
fn text<'a>(v: &'a IvyJson, k: &str) -> IvyResult<&'a JsString> {
    field(v, k)?.as_js_string().ok_or_else(IvyError::internal)
}
impl PhoneNativeClient {
    async fn observed(
        &self,
        method: &str,
        args: IvyJson,
        operation: Option<JsString>,
        timeout: u64,
        shape: &str,
        message: &str,
    ) -> IvyResult<IvyJson> {
        let reply = self
            .request(method, args, operation, timeout)?
            .wait()
            .await?;
        require(
            reply.get("ok") == Some(&true.into()) && !self.is_closed(),
            "phone_unavailable",
            message,
        )?;
        let result = field(&reply, "result")?.clone();
        validate_native(shape, &result)?;
        Ok(result)
    }
    pub async fn observe(&self, call_id: Option<JsString>) -> IvyResult<IvyJson> {
        let args = call_id.filter(|id| !id.0.is_empty()).map_or_else(
            || IvyJson::object([] as [(&str, IvyJson); 0]),
            |id| IvyJson::object([("callId", id.into())]),
        );
        let status = self
            .observed(
                "status",
                args,
                None,
                5000,
                "Status",
                "Original native Phone status is unavailable.",
            )
            .await?;
        let call = field(&status, "call")?;
        let media = field(&status, "media")?;
        let audio = media.get("audio").filter(|v| !v.is_null());
        require(
            call.is_null() == media.is_null()
                && audio.is_none_or(|a| a.get("callId") == call.get("id"))
                && (status.get("configured") == Some(&true.into())
                    || call.is_null()
                        && status.get("registration").is_some_and(IvyJson::is_null)
                        && status.get("endpoint").is_some_and(IvyJson::is_null)),
            "phone_invalid_status",
            "Native status has inconsistent call or configuration ownership.",
        )?;
        if !call.is_null() {
            require(
                if call.get("direction") == Some(&"incoming".into()) {
                    call.get("incoming").is_some_and(|v| {
                        !v.is_null() && v.get("sipCallId") == call.get("sipCallId")
                    })
                } else {
                    call.get("incoming").is_some_and(IvyJson::is_null)
                },
                "phone_invalid_status",
                "Native status has inconsistent original caller context.",
            )?;
        }
        Ok(status)
    }
    pub async fn observe_desktop(&self, app_user_model_id: JsString) -> IvyResult<IvyJson> {
        let result = self
            .observed(
                "desktop.observe",
                IvyJson::object([("appUserModelId", app_user_model_id.clone().into())]),
                None,
                5000,
                "DesktopObservation",
                "Original Desktop observation is unavailable.",
            )
            .await?;
        require(
            result.get("identity").is_none_or(|v| {
                v.is_null() || v.get("appUserModelId") == Some(&app_user_model_id.into())
            }),
            "phone_invalid_status",
            "Observed Desktop belongs to another configured application.",
        )?;
        Ok(result)
    }
    pub async fn launch_desktop(
        &self,
        application: IvyJson,
        operation: JsString,
    ) -> IvyResult<IvyJson> {
        let result = self
            .observed(
                "desktop.launch",
                application.clone(),
                Some(operation),
                15000,
                "LaunchResult",
                "Desktop activation did not return a confirmed result.",
            )
            .await?;
        require(
            result.get("identity").is_none_or(|v| {
                v.is_null() || v.get("appUserModelId") == application.get("appUserModelId")
            }),
            "phone_invalid_status",
            "Desktop activation returned another configured application.",
        )?;
        Ok(result)
    }
    pub async fn observe_controls(
        &self,
        desktop: IvyJson,
        labels: Vec<JsString>,
    ) -> IvyResult<IvyJson> {
        let result = self
            .observed(
                "desktop.controls",
                IvyJson::object([
                    ("desktop", desktop),
                    (
                        "labels",
                        IvyJson::Array(labels.iter().cloned().map(Into::into).collect()),
                    ),
                ]),
                None,
                5000,
                "Observation",
                "Original Desktop controls are unavailable.",
            )
            .await?;
        let mut windows = BTreeSet::new();
        let mut ids = BTreeSet::new();
        for window in field(&result, "windows")?
            .as_array()
            .ok_or_else(IvyError::internal)?
        {
            require(
                windows.insert(text(window, "handle")?.clone()),
                "phone_invalid_status",
                "Duplicate Desktop window observation.",
            )?;
            for control in field(window, "controls")?
                .as_array()
                .ok_or_else(IvyError::internal)?
            {
                let id = canonical(field(control, "id")?, FRAME_BYTES)?;
                require(
                    ids.insert(id.clone())
                        && id != canonical(field(control, "parentId")?, FRAME_BYTES)?
                        && labels.contains(text(control, "name")?),
                    "phone_invalid_status",
                    "Desktop control identity or label differs from the original scoped observation.",
                )?;
            }
        }
        Ok(result)
    }
    pub async fn observe_capture(&self, endpoint_id: JsString) -> IvyResult<IvyJson> {
        let result = self
            .observed(
                "desktop.capture",
                IvyJson::object([("endpointId", endpoint_id.clone().into())]),
                None,
                5000,
                "CaptureObservation",
                "Original capture observation is unavailable.",
            )
            .await?;
        let mut ids = BTreeSet::new();
        let sessions = field(&result, "sessions")?
            .as_array()
            .ok_or_else(IvyError::internal)?;
        require(
            result.get("endpointId") == Some(&endpoint_id.into())
                && sessions.iter().all(|s| {
                    s.get("instanceId")
                        .and_then(IvyJson::as_js_string)
                        .is_some_and(|id| ids.insert(id.clone()))
                        && (s.get("singleProcess") != Some(&true.into())
                            || s.get("pid")
                                .and_then(IvyJson::as_f64)
                                .is_some_and(|pid| pid > 0.0))
                }),
            "phone_invalid_status",
            "Capture observation belongs to another endpoint or repeats a session.",
        )?;
        Ok(result)
    }
    pub async fn observe_audio_process(&self, desktop: IvyJson, pid: f64) -> IvyResult<IvyJson> {
        let result = self
            .observed(
                "desktop.process",
                IvyJson::object([("desktop", desktop), ("pid", pid.into())]),
                None,
                5000,
                "ProcessOwnership",
                "Original audio-process observation is unavailable.",
            )
            .await?;
        require(
            result.get("pid") == Some(&pid.into()),
            "phone_invalid_status",
            "Audio-process observation belongs to another process.",
        )?;
        Ok(result)
    }
    pub async fn observe_desktop_capture(
        &self,
        desktop: IvyJson,
        endpoint_id: JsString,
    ) -> IvyResult<IvyJson> {
        let result = self
            .observed(
                "desktop.captureOwner",
                IvyJson::object([
                    ("desktop", desktop),
                    ("endpointId", endpoint_id.clone().into()),
                ]),
                None,
                5000,
                "DesktopCaptureCorrelation",
                "Current Desktop capture correlation is unavailable.",
            )
            .await?;
        require(
            result.get("endpointId") == Some(&endpoint_id.into()),
            "phone_invalid_status",
            "Capture correlation belongs to another endpoint.",
        )?;
        Ok(result)
    }
}
