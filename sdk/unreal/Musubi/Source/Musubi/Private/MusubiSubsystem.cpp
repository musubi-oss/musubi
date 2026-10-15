#include "MusubiSubsystem.h"
#include "MusubiModule.h"
#include "Serialization/JsonReader.h"
#include "Serialization/JsonSerializer.h"

void UMusubiSubsystem::Initialize(FSubsystemCollectionBase& Collection)
{
    Super::Initialize(Collection);
    Rt = MakeShared<FMusubiSocket>();
    TWeakObjectPtr<UMusubiSubsystem> Weak(this);
    Rt->OnConnected.AddLambda([Weak]() { if (Weak.IsValid()) Weak->OnSocketConnected.Broadcast(); });
    Rt->OnClosed.AddLambda([Weak](int32 Code, const FString& Reason) { if (Weak.IsValid()) Weak->OnSocketClosed.Broadcast(Code, Reason); });
    Rt->OnMatchData.AddLambda([Weak](const FMusubiMatchData& D) { if (Weak.IsValid()) Weak->OnMatchData.Broadcast(D); });
    Rt->OnWorldDelta.AddLambda([Weak](const FMusubiWorldDelta& D) { if (Weak.IsValid()) Weak->OnWorldDelta.Broadcast(D); });
    Rt->OnMatchmakerMatched.AddLambda([Weak](const FMusubiMatchmakerMatched& M) { if (Weak.IsValid()) Weak->OnMatchmakerMatched.Broadcast(M); });
}

void UMusubiSubsystem::Deinitialize()
{
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
    Rt->Connect(FMusubiClient::WebSocketUrl(Http->BaseUrl(), Http->Session().AccessToken, Status));
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
    if (Rt.IsValid()) Rt->MatchDataSend(MatchId, OpCode, Data, bReliable);
}

void UMusubiSubsystem::SubscribeWorld(const FString& World, double X, double Y, double Radius)
{
    if (Rt.IsValid()) Rt->WorldSubscribe(World, X, Y, Radius);
}

void UMusubiSubsystem::SendWorldInput(const FString& World, const FString& EntityId, const FString& InputJson)
{
    if (!Rt.IsValid()) return;
    TSharedPtr<FJsonObject> Input;
    TSharedRef<TJsonReader<>> R = TJsonReaderFactory<>::Create(InputJson);
    FJsonSerializer::Deserialize(R, Input);
    Rt->WorldInput(World, EntityId, Input);
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
