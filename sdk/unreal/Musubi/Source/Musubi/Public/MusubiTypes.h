#pragma once

#include "CoreMinimal.h"
#include "MusubiTypes.generated.h"

/** Result of device / email authentication (`POST /v1/auth/device|email|refresh`). */
USTRUCT(BlueprintType)
struct MUSUBI_API FMusubiSession
{
    GENERATED_BODY()

    UPROPERTY(BlueprintReadOnly, Category = "Musubi") FString AccountId;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") FString SessionId;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") FString AccessToken;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") FString RefreshToken;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") bool bCreated = false;

    bool IsValid() const { return !AccessToken.IsEmpty(); }
};

USTRUCT(BlueprintType)
struct MUSUBI_API FMusubiPresence
{
    GENERATED_BODY()

    UPROPERTY(BlueprintReadOnly, Category = "Musubi") FString AccountId;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") FString SessionId;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") FString Username;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") FString Status;
};

/** `match_data` from the server (authoritative or relayed match). */
USTRUCT(BlueprintType)
struct MUSUBI_API FMusubiMatchData
{
    GENERATED_BODY()

    UPROPERTY(BlueprintReadOnly, Category = "Musubi") FString MatchId;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") FMusubiPresence Sender;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") int64 OpCode = 0;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") TArray<uint8> Data;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") bool bReliable = true;
};

USTRUCT(BlueprintType)
struct MUSUBI_API FMusubiWorldComponent
{
    GENERATED_BODY()

    UPROPERTY(BlueprintReadOnly, Category = "Musubi") FString Name;
    /** JSON text of the component value (decode with your own schema). */
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") FString ValueJson;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") int64 Version = 0;
};

/** Entity inside a `world_delta` (spawned or updated; only changed components for updates). */
USTRUCT(BlueprintType)
struct MUSUBI_API FMusubiWorldEntity
{
    GENERATED_BODY()

    UPROPERTY(BlueprintReadOnly, Category = "Musubi") FString Id;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") double X = 0.0;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") double Y = 0.0;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") TArray<FMusubiWorldComponent> Components;
};

/** Server meshing delta: everything that changed inside the interest radius since the last one. */
USTRUCT(BlueprintType)
struct MUSUBI_API FMusubiWorldDelta
{
    GENERATED_BODY()

    UPROPERTY(BlueprintReadOnly, Category = "Musubi") FString World;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") int64 Tick = 0;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") TArray<FMusubiWorldEntity> Spawned;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") TArray<FMusubiWorldEntity> Updated;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") TArray<FString> Despawned;
};

/** Dedicated server handed out by the matchmaker (tickets carried `fleet=<name>`). */
USTRUCT(BlueprintType)
struct MUSUBI_API FMusubiMatchmakerServer
{
    GENERATED_BODY()

    UPROPERTY(BlueprintReadOnly, Category = "Musubi") FString ServerId;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") FString Address;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") int32 Port = 0;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") FString ConnectToken;
};

USTRUCT(BlueprintType)
struct MUSUBI_API FMusubiMatchmakerMatched
{
    GENERATED_BODY()

    UPROPERTY(BlueprintReadOnly, Category = "Musubi") FString Ticket;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") FString Token;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") TArray<FMusubiPresence> Users;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") bool bHasServer = false;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") FMusubiMatchmakerServer Server;
};

/** Aurix voice grant from `POST /v1/voice/token`. */
USTRUCT(BlueprintType)
struct MUSUBI_API FMusubiVoiceGrant
{
    GENERATED_BODY()

    UPROPERTY(BlueprintReadOnly, Category = "Musubi") FString Token;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") FString UserId;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") FString ChannelId;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") FString ExpiresAt;
    /** JSON text of Aurix's `endpoint` object (signalling URL etc.). */
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") FString EndpointJson;
};

USTRUCT(BlueprintType)
struct MUSUBI_API FMusubiError
{
    GENERATED_BODY()

    UPROPERTY(BlueprintReadOnly, Category = "Musubi") FString Code;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") FString Message;
    UPROPERTY(BlueprintReadOnly, Category = "Musubi") int32 HttpStatus = 0;
};
