#include "MusubiSubsystem.h"
#include "MusubiSettings.h"
#include "MusubiModule.h"
#include "Containers/Ticker.h"
#include "Serialization/JsonReader.h"
#include "Serialization/JsonSerializer.h"

void UMusubiSubsystem::Initialize(FSubsystemCollectionBase& Collection)
{
    Super::Initialize(Collection);
    Rt = MakeShared<FMusubiSocket>();
    TWeakObjectPtr<UMusubiSubsystem> Weak(this);
    Rt->OnStateChanged.AddLambda([Weak](EMusubiSocketState S)
    {
        if (!Weak.IsValid() || S != EMusubiSocketState::Open) return;
        if (!Weak->PendingStatus.IsEmpty())
        {
            FMusubiClientMessage M;
            M.Tag = EMusubiClientMessageTag::StatusUpdate;
            FMusubiClientMessageStatusUpdate Body;
            Body.status = Weak->PendingStatus;
            M.StatusUpdate = MoveTemp(Body);
            Weak->Rt->Send(M);
        }
        Weak->OnSocketConnected.Broadcast();
    });
    Rt->OnClosed.AddLambda([Weak](int32 Code, const FString& Reason) { if (Weak.IsValid()) Weak->OnSocketClosed.Broadcast(Code, Reason); });
    // Token may have been refreshed while the socket was down.
    Rt->OnResubscribe.AddLambda([Weak]() { if (Weak.IsValid() && Weak->Http.IsValid()) Weak->Rt->SetAccessToken(Weak->Http->Session().AccessToken); });
    TickHandle = FTSTicker::GetCoreTicker().AddTicker(FTickerDelegate::CreateLambda([Weak](float Dt)
    {
        if (Weak.IsValid() && Weak->Rt.IsValid()) Weak->Rt->Tick(Dt);
        return Weak.IsValid();
    }));
    // Project Settings → Plugins → Musubi: socket tuning and, when set, the node address.
    if (const UMusubiSettings* S = GetDefault<UMusubiSettings>())
    {
        SockCfg = S->SocketConfig();
        const FString Api = S->EffectiveApiBase();
        if (!Api.IsEmpty()) Configure(Api);
    }
}

void UMusubiSubsystem::Deinitialize()
{
    FTSTicker::GetCoreTicker().RemoveTicker(TickHandle);
    if (Rt.IsValid()) Rt->Close();
    Rt.Reset();
    Http.Reset();
    Super::Deinitialize();
}

void UMusubiSubsystem::Configure(const FString& BaseUrl)
{
    Http = MakeShared<FMusubiClient>(BaseUrl);
    TWeakObjectPtr<UMusubiSubsystem> Weak(this);
    Http->OnError.BindLambda([Weak](const FMusubiError& E) { if (Weak.IsValid()) Weak->OnError.Broadcast(E); });
}

void UMusubiSubsystem::AuthenticateDevice(const FString& DeviceId, bool bCreate, const FString& Username)
{
    if (!Http.IsValid()) { UE_LOG(LogMusubi, Error, TEXT("Configure() first")); return; }
    TWeakObjectPtr<UMusubiSubsystem> Weak(this);
    Http->AuthenticateDevice(DeviceId, bCreate, Username, FMusubiSessionResult::CreateLambda([Weak](bool bOk, const FMusubiSession& S)
    {
        if (Weak.IsValid()) Weak->OnSession.Broadcast(bOk, S);
    }));
}

void UMusubiSubsystem::AuthenticateEmail(const FString& Email, const FString& Password, bool bCreate, const FString& Username)
{
    if (!Http.IsValid()) { UE_LOG(LogMusubi, Error, TEXT("Configure() first")); return; }
    TWeakObjectPtr<UMusubiSubsystem> Weak(this);
    Http->AuthenticateEmail(Email, Password, bCreate, Username, FMusubiSessionResult::CreateLambda([Weak](bool bOk, const FMusubiSession& S)
    {
        if (Weak.IsValid()) Weak->OnSession.Broadcast(bOk, S);
    }));
}

FMusubiSession UMusubiSubsystem::GetSession() const
{
    return Http.IsValid() ? Http->Session() : FMusubiSession();
}

void UMusubiSubsystem::ConnectSocket(const FString& Status)
{
    if (!Http.IsValid() || !Http->HasSession()) { UE_LOG(LogMusubi, Error, TEXT("authenticate first")); return; }
    PendingStatus = Status;
    Rt->Connect(Http->BaseUrl(), Http->Session().AccessToken, SockCfg);
}

void UMusubiSubsystem::DisconnectSocket()
{
    if (Rt.IsValid()) Rt->Close();
}

bool UMusubiSubsystem::IsSocketConnected() const
{
    return Rt.IsValid() && Rt->IsConnected();
}

void UMusubiSubsystem::SendMatchData(const FString& MatchId, int64 OpCode, const TArray<uint8>& Data, bool bReliable)
{
    if (!Rt.IsValid()) return;
    FMusubiClientMessage M;
    M.Tag = EMusubiClientMessageTag::MatchDataSend;
    FMusubiClientMessageMatchDataSend Body;
    Body.match_id = MatchId;
    Body.op_code = OpCode;
    Body.data = Data;
    Body.reliable = bReliable;
    M.MatchDataSend = MoveTemp(Body);
    Rt->Send(M);
}

void UMusubiSubsystem::RequestVoiceToken(const FString& PartyId, const FString& MatchId, const FString& DisplayName)
{
    if (!Http.IsValid()) return;
    TWeakObjectPtr<UMusubiSubsystem> Weak(this);
    Http->GetVoiceToken(PartyId, MatchId, DisplayName, FMusubiVoiceResult::CreateLambda([Weak](bool bOk, const FMusubiVoiceGrant& G)
    {
        if (Weak.IsValid()) Weak->OnVoiceGrant.Broadcast(bOk, G);
    }));
}
