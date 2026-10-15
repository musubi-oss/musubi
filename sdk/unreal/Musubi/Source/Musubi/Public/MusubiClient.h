#pragma once

#include "CoreMinimal.h"
#include "Dom/JsonObject.h"
#include "Interfaces/IHttpRequest.h"
#include "MusubiTypes.h"

DECLARE_DELEGATE_TwoParams(FMusubiSessionResult, bool /*bOk*/, const FMusubiSession& /*Session*/);
DECLARE_DELEGATE_TwoParams(FMusubiJsonResult, bool /*bOk*/, TSharedPtr<FJsonObject> /*Body*/);
DECLARE_DELEGATE_TwoParams(FMusubiVoiceResult, bool /*bOk*/, const FMusubiVoiceGrant& /*Grant*/);
DECLARE_DELEGATE_OneParam(FMusubiErrorEvent, const FMusubiError& /*Error*/);

/**
 * HTTP client for the Musubi REST API. Owns the session (access + refresh token) and refreshes it
 * transparently when a call answers 401. Thread: game thread only.
 */
class MUSUBI_API FMusubiClient : public TSharedFromThis<FMusubiClient>
{
public:
    explicit FMusubiClient(const FString& InBaseUrl);

    const FString& BaseUrl() const { return Base; }
    const FMusubiSession& Session() const { return Current; }
    bool HasSession() const { return Current.IsValid(); }
    void SetSession(const FMusubiSession& S) { Current = S; }

    /** `POST /v1/auth/device {id, create, username}` — the usual first call on a device. */
    void AuthenticateDevice(const FString& DeviceId, bool bCreate, const FString& Username, FMusubiSessionResult OnDone);
    /** `POST /v1/auth/email {email, password, create, username}`. */
    void AuthenticateEmail(const FString& Email, const FString& Password, bool bCreate, const FString& Username, FMusubiSessionResult OnDone);
    /** `POST /v1/auth/refresh {refresh_token}` — rotates the pair (reuse of an old one revokes the family). */
    void Refresh(FMusubiSessionResult OnDone);
    /** `POST /v1/auth/logout`. */
    void Logout(FMusubiJsonResult OnDone);

    /** `GET /v1/account/me`. */
    void GetAccount(FMusubiJsonResult OnDone);
    /** `POST /v1/rpc/{id}` with a JSON payload (server runtime / WASM module). */
    void Rpc(const FString& Id, const TSharedPtr<FJsonObject>& Payload, FMusubiJsonResult OnDone);
    /** `GET /v1/liveops/flags?names=a,b` (Zanshin). */
    void GetFlags(const TArray<FString>& Names, FMusubiJsonResult OnDone);
    /** `POST /v1/voice/token` for a party or a match (Aurix). */
    void GetVoiceToken(const FString& PartyId, const FString& MatchId, const FString& DisplayName, FMusubiVoiceResult OnDone);

    /** Generic authenticated call; retries once after a successful refresh on 401. */
    void Call(const FString& Verb, const FString& Path, const TSharedPtr<FJsonObject>& Body, FMusubiJsonResult OnDone, bool bAllowRefresh = true);

    /** Fired for every failed request (after refresh retry). */
    FMusubiErrorEvent OnError;

    static FString WebSocketUrl(const FString& HttpBaseUrl, const FString& AccessToken, const FString& Status = TEXT(""));

private:
    void Auth(const FString& Path, const TSharedRef<FJsonObject>& Body, FMusubiSessionResult OnDone);
    void Send(const FString& Verb, const FString& Path, const FString& Body, bool bBearer, TFunction<void(int32, TSharedPtr<FJsonObject>)> OnDone);
    static FMusubiSession ParseSession(const TSharedPtr<FJsonObject>& O);
    static FString ToJson(const TSharedPtr<FJsonObject>& O);

    FString Base;
    FMusubiSession Current;
};
