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

/** `match_data` from the server (authoritative or relayed match). */
/** Entity inside a `world_delta` (spawned or updated; only changed components for updates). */
/** Server meshing delta: everything that changed inside the interest radius since the last one. */
/** Dedicated server handed out by the matchmaker (tickets carried `fleet=<name>`). */
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
