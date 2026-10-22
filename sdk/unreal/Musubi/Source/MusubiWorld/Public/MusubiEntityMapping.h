// Which actor class / mesh an entity becomes, decided by its components (`kind=player → BP_RemotePlayer`).
#pragma once

#include "CoreMinimal.h"
#include "Engine/DataAsset.h"
#include "MusubiWorldTypes.h"
#include "MusubiEntityMapping.generated.h"

USTRUCT(BlueprintType)
struct MUSUBIWORLD_API FMusubiEntityRule
{
	GENERATED_BODY()

	/** Component that must be present (e.g. `kind`). Empty = matches every entity (use as the last rule). */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi")
	FString Component;

	/** If set, the component's JSON value (string compared as text, numbers/bools as their JSON text) must equal this. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi")
	FString Equals;

	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi")
	EMusubiEntitySink Sink = EMusubiEntitySink::Actor;

	/** Actor sink: class to spawn. Needs a UMusubiEntityComponent (added at spawn if missing). */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi", meta = (EditCondition = "Sink == EMusubiEntitySink::Actor"))
	TSubclassOf<AActor> ActorClass;

	/** Instanced sink: mesh to instance. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi", meta = (EditCondition = "Sink == EMusubiEntitySink::Instanced"))
	TObjectPtr<UStaticMesh> Mesh;

	/** Instanced sink: uniform scale of the instance. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi", meta = (EditCondition = "Sink == EMusubiEntitySink::Instanced"))
	float Scale = 1.f;
};

UCLASS(BlueprintType)
class MUSUBIWORLD_API UMusubiEntityMapping : public UDataAsset
{
	GENERATED_BODY()
public:
	/** First matching rule wins. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi")
	TArray<FMusubiEntityRule> Rules;

	/** Used when no rule matches; null = EMusubiEntitySink::None. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Musubi")
	TSubclassOf<AActor> DefaultActorClass;

	/** Returns the rule for an entity, or null for the default. */
	const FMusubiEntityRule* Resolve(const FMusubiWorldEntity& E) const;
};
