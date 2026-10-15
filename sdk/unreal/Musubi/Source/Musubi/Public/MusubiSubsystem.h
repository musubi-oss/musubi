#pragma once

#include "CoreMinimal.h"
#include "MusubiClient.h"
#include "MusubiSocket.h"
#include "Subsystems/GameInstanceSubsystem.h"
#include "MusubiSubsystem.generated.h"

DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FMusubiOnSession, bool, bOk, const FMusubiSession&, Session);
DECLARE_DYNAMIC_MULTICAST_DELEGATE(FMusubiOnSocketConnected);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FMusubiOnSocketClosed, int32, Code, const FString&, Reason);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam(FMusubiOnMatchData, const FMusubiMatchData&, Data);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam(FMusubiOnWorldDelta, const FMusubiWorldDelta&, Delta);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam(FMusubiOnMatchmakerMatched, const FMusubiMatchmakerMatched&, Matched);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FMusubiOnVoiceGrant, bool, bOk, const FMusubiVoiceGrant&, Grant);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam(FMusubiOnError, const FMusubiError&, Error);

/** Blueprint-facing entry point: one client + one socket per game instance. */
UCLASS()
class MUSUBI_API UMusubiSubsystem : public UGameInstanceSubsystem
{
    GENERATED_BODY()

public:
    virtual void Initialize(FSubsystemCollectionBase& Collection) override;
    virtual void Deinitialize() override;

    /** e.g. "http://localhost:7350". Call before anything else. */
    UFUNCTION(BlueprintCallable, Category = "Musubi")
    void Configure(const FString& BaseUrl);

    UFUNCTION(BlueprintCallable, Category = "Musubi|Auth")
    void AuthenticateDevice(const FString& DeviceId, bool bCreate = true, const FString& Username = TEXT(""));

    UFUNCTION(BlueprintCallable, Category = "Musubi|Auth")
    void AuthenticateEmail(const FString& Email, const FString& Password, bool bCreate = true, const FString& Username = TEXT(""));

    UFUNCTION(BlueprintPure, Category = "Musubi|Auth")
    FMusubiSession GetSession() const;

    /** Open the realtime socket with the current session (fires OnSocketConnected / OnSocketClosed). */
    UFUNCTION(BlueprintCallable, Category = "Musubi|Realtime")
    void ConnectSocket(const FString& Status = TEXT(""));

    UFUNCTION(BlueprintCallable, Category = "Musubi|Realtime")
    void DisconnectSocket();

    UFUNCTION(BlueprintPure, Category = "Musubi|Realtime")
    bool IsSocketConnected() const;

    UFUNCTION(BlueprintCallable, Category = "Musubi|Match")
    void SendMatchData(const FString& MatchId, int64 OpCode, const TArray<uint8>& Data, bool bReliable = true);

    UFUNCTION(BlueprintCallable, Category = "Musubi|World")
    void SubscribeWorld(const FString& World, double X, double Y, double Radius);

    UFUNCTION(BlueprintCallable, Category = "Musubi|World")
    void SendWorldInput(const FString& World, const FString& EntityId, const FString& InputJson);

    /** Voice (Aurix) token for a party or a match; result arrives on OnVoiceGrant. */
    UFUNCTION(BlueprintCallable, Category = "Musubi|Voice")
    void RequestVoiceToken(const FString& PartyId, const FString& MatchId, const FString& DisplayName);

    UPROPERTY(BlueprintAssignable, Category = "Musubi") FMusubiOnSession OnSession;
    UPROPERTY(BlueprintAssignable, Category = "Musubi") FMusubiOnSocketConnected OnSocketConnected;
    UPROPERTY(BlueprintAssignable, Category = "Musubi") FMusubiOnSocketClosed OnSocketClosed;
    UPROPERTY(BlueprintAssignable, Category = "Musubi") FMusubiOnMatchData OnMatchData;
    UPROPERTY(BlueprintAssignable, Category = "Musubi") FMusubiOnWorldDelta OnWorldDelta;
    UPROPERTY(BlueprintAssignable, Category = "Musubi") FMusubiOnMatchmakerMatched OnMatchmakerMatched;
    UPROPERTY(BlueprintAssignable, Category = "Musubi") FMusubiOnVoiceGrant OnVoiceGrant;
    UPROPERTY(BlueprintAssignable, Category = "Musubi") FMusubiOnError OnError;

    /** Full C++ API. */
    TSharedPtr<FMusubiClient> Client() const { return Http; }
    TSharedPtr<FMusubiSocket> Socket() const { return Rt; }

private:
    TSharedPtr<FMusubiClient> Http;
    TSharedPtr<FMusubiSocket> Rt;
};
