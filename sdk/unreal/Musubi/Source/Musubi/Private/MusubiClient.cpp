#include "MusubiClient.h"
#include "HttpModule.h"
#include "Interfaces/IHttpResponse.h"
#include "MusubiModule.h"
#include "Serialization/JsonReader.h"
#include "Serialization/JsonSerializer.h"
#include "Serialization/JsonWriter.h"

FMusubiClient::FMusubiClient(const FString& InBaseUrl)
{
    Base = InBaseUrl;
    while (Base.EndsWith(TEXT("/"))) Base.LeftChopInline(1);
}

FString FMusubiClient::ToJson(const TSharedPtr<FJsonObject>& O)
{
    FString Out;
    if (O.IsValid())
    {
        TSharedRef<TJsonWriter<>> W = TJsonWriterFactory<>::Create(&Out);
        FJsonSerializer::Serialize(O.ToSharedRef(), W);
    }
    return Out;
}

FMusubiSession FMusubiClient::ParseSession(const TSharedPtr<FJsonObject>& O)
{
    FMusubiSession S;
    if (!O.IsValid()) return S;
    O->TryGetStringField(TEXT("account_id"), S.AccountId);
    O->TryGetStringField(TEXT("session_id"), S.SessionId);
    O->TryGetStringField(TEXT("access_token"), S.AccessToken);
    O->TryGetStringField(TEXT("refresh_token"), S.RefreshToken);
    O->TryGetBoolField(TEXT("created"), S.bCreated);
    return S;
}

FString FMusubiClient::WebSocketUrl(const FString& HttpBaseUrl, const FString& AccessToken, const FString& Status)
{
    FString Ws = HttpBaseUrl;
    if (Ws.StartsWith(TEXT("https://"))) Ws = TEXT("wss://") + Ws.RightChop(8);
    else if (Ws.StartsWith(TEXT("http://"))) Ws = TEXT("ws://") + Ws.RightChop(7);
    while (Ws.EndsWith(TEXT("/"))) Ws.LeftChopInline(1);
    Ws += TEXT("/ws?format=json&token=") + FGenericPlatformHttp::UrlEncode(AccessToken);
    if (!Status.IsEmpty()) Ws += TEXT("&status=") + FGenericPlatformHttp::UrlEncode(Status);
    return Ws;
}

void FMusubiClient::Send(const FString& Verb, const FString& Path, const FString& Body, bool bBearer, TFunction<void(int32, TSharedPtr<FJsonObject>)> OnDone)
{
    TSharedRef<IHttpRequest, ESPMode::ThreadSafe> Req = FHttpModule::Get().CreateRequest();
    Req->SetURL(Base + Path);
    Req->SetVerb(Verb);
    Req->SetHeader(TEXT("Accept"), TEXT("application/json"));
    if (!Body.IsEmpty())
    {
        Req->SetHeader(TEXT("Content-Type"), TEXT("application/json"));
        Req->SetContentAsString(Body);
    }
    if (bBearer && Current.IsValid())
    {
        Req->SetHeader(TEXT("Authorization"), TEXT("Bearer ") + Current.AccessToken);
    }
    Req->OnProcessRequestComplete().BindLambda([OnDone](FHttpRequestPtr, FHttpResponsePtr Res, bool bConnected)
    {
        if (!bConnected || !Res.IsValid())
        {
            OnDone(0, nullptr);
            return;
        }
        TSharedPtr<FJsonObject> O;
        TSharedRef<TJsonReader<>> R = TJsonReaderFactory<>::Create(Res->GetContentAsString());
        FJsonSerializer::Deserialize(R, O);
        OnDone(Res->GetResponseCode(), O);
    });
    Req->ProcessRequest();
}

void FMusubiClient::Call(const FString& Verb, const FString& Path, const TSharedPtr<FJsonObject>& Body, FMusubiJsonResult OnDone, bool bAllowRefresh)
{
    TWeakPtr<FMusubiClient> Weak = AsShared();
    Send(Verb, Path, ToJson(Body), true, [Weak, Verb, Path, Body, OnDone, bAllowRefresh](int32 Status, TSharedPtr<FJsonObject> O)
    {
        TSharedPtr<FMusubiClient> Self = Weak.Pin();
        if (!Self.IsValid()) return;
        if (Status == 401 && bAllowRefresh && !Self->Current.RefreshToken.IsEmpty())
        {
            Self->Refresh(FMusubiSessionResult::CreateLambda([Weak, Verb, Path, Body, OnDone](bool bOk, const FMusubiSession&)
            {
                TSharedPtr<FMusubiClient> Again = Weak.Pin();
                if (!Again.IsValid()) return;
                if (bOk) Again->Call(Verb, Path, Body, OnDone, false);
                else OnDone.ExecuteIfBound(false, nullptr);
            }));
            return;
        }
        const bool bOk = Status >= 200 && Status < 300;
        if (!bOk)
        {
            FMusubiError E;
            E.HttpStatus = Status;
            if (O.IsValid())
            {
                O->TryGetStringField(TEXT("code"), E.Code);
                O->TryGetStringField(TEXT("message"), E.Message);
            }
            UE_LOG(LogMusubi, Warning, TEXT("%s %s -> %d %s: %s"), *Verb, *Path, Status, *E.Code, *E.Message);
            Self->OnError.ExecuteIfBound(E);
        }
        OnDone.ExecuteIfBound(bOk, O);
    });
}

void FMusubiClient::Auth(const FString& Path, const TSharedRef<FJsonObject>& Body, FMusubiSessionResult OnDone)
{
    TWeakPtr<FMusubiClient> Weak = AsShared();
    Send(TEXT("POST"), Path, ToJson(Body), false, [Weak, OnDone, Path](int32 Status, TSharedPtr<FJsonObject> O)
    {
        TSharedPtr<FMusubiClient> Self = Weak.Pin();
        if (!Self.IsValid()) return;
        const bool bOk = Status >= 200 && Status < 300 && O.IsValid();
        if (bOk) Self->Current = ParseSession(O);
        else
        {
            FMusubiError E;
            E.HttpStatus = Status;
            if (O.IsValid())
            {
                O->TryGetStringField(TEXT("code"), E.Code);
                O->TryGetStringField(TEXT("message"), E.Message);
            }
            UE_LOG(LogMusubi, Warning, TEXT("auth %s -> %d %s"), *Path, Status, *E.Message);
            Self->OnError.ExecuteIfBound(E);
        }
        OnDone.ExecuteIfBound(bOk, Self->Current);
    });
}

void FMusubiClient::AuthenticateDevice(const FString& DeviceId, bool bCreate, const FString& Username, FMusubiSessionResult OnDone)
{
    TSharedRef<FJsonObject> B = MakeShared<FJsonObject>();
    B->SetStringField(TEXT("id"), DeviceId);
    B->SetBoolField(TEXT("create"), bCreate);
    if (!Username.IsEmpty()) B->SetStringField(TEXT("username"), Username);
    Auth(TEXT("/v1/auth/device"), B, OnDone);
}

void FMusubiClient::AuthenticateEmail(const FString& Email, const FString& Password, bool bCreate, const FString& Username, FMusubiSessionResult OnDone)
{
    TSharedRef<FJsonObject> B = MakeShared<FJsonObject>();
    B->SetStringField(TEXT("email"), Email);
    B->SetStringField(TEXT("password"), Password);
    B->SetBoolField(TEXT("create"), bCreate);
    if (!Username.IsEmpty()) B->SetStringField(TEXT("username"), Username);
    Auth(TEXT("/v1/auth/email"), B, OnDone);
}

void FMusubiClient::Refresh(FMusubiSessionResult OnDone)
{
    TSharedRef<FJsonObject> B = MakeShared<FJsonObject>();
    B->SetStringField(TEXT("refresh_token"), Current.RefreshToken);
    Auth(TEXT("/v1/auth/refresh"), B, OnDone);
}

void FMusubiClient::Logout(FMusubiJsonResult OnDone)
{
    TSharedPtr<FJsonObject> B = MakeShared<FJsonObject>();
    B->SetStringField(TEXT("refresh_token"), Current.RefreshToken);
    Call(TEXT("POST"), TEXT("/v1/auth/logout"), B, FMusubiJsonResult::CreateLambda([this, OnDone](bool bOk, TSharedPtr<FJsonObject> O)
    {
        Current = FMusubiSession();
        OnDone.ExecuteIfBound(bOk, O);
    }), false);
}

void FMusubiClient::GetAccount(FMusubiJsonResult OnDone)
{
    Call(TEXT("GET"), TEXT("/v1/account/me"), nullptr, OnDone);
}

void FMusubiClient::Rpc(const FString& Id, const TSharedPtr<FJsonObject>& Payload, FMusubiJsonResult OnDone)
{
    Call(TEXT("POST"), TEXT("/v1/rpc/") + Id, Payload, OnDone);
}

void FMusubiClient::GetFlags(const TArray<FString>& Names, FMusubiJsonResult OnDone)
{
    FString Path = TEXT("/v1/liveops/flags");
    if (Names.Num() > 0) Path += TEXT("?names=") + FGenericPlatformHttp::UrlEncode(FString::Join(Names, TEXT(",")));
    Call(TEXT("GET"), Path, nullptr, OnDone);
}

void FMusubiClient::GetVoiceToken(const FString& PartyId, const FString& MatchId, const FString& DisplayName, FMusubiVoiceResult OnDone)
{
    TSharedPtr<FJsonObject> B = MakeShared<FJsonObject>();
    if (!PartyId.IsEmpty()) B->SetStringField(TEXT("party_id"), PartyId);
    if (!MatchId.IsEmpty()) B->SetStringField(TEXT("match_id"), MatchId);
    if (!DisplayName.IsEmpty()) B->SetStringField(TEXT("display_name"), DisplayName);
    Call(TEXT("POST"), TEXT("/v1/voice/token"), B, FMusubiJsonResult::CreateLambda([OnDone](bool bOk, TSharedPtr<FJsonObject> O)
    {
        FMusubiVoiceGrant G;
        if (bOk && O.IsValid())
        {
            O->TryGetStringField(TEXT("token"), G.Token);
            O->TryGetStringField(TEXT("user_id"), G.UserId);
            O->TryGetStringField(TEXT("channel_id"), G.ChannelId);
            O->TryGetStringField(TEXT("expires_at"), G.ExpiresAt);
            const TSharedPtr<FJsonObject>* Ep = nullptr;
            if (O->TryGetObjectField(TEXT("endpoint"), Ep) && Ep) G.EndpointJson = ToJson(*Ep);
        }
        OnDone.ExecuteIfBound(bOk, G);
    }));
}
