#include "MusubiVehicleHandover.h"
#include "ChaosWheeledVehicleMovementComponent.h"
#include "Dom/JsonObject.h"
#include "MusubiServerModule.h"

static UChaosWheeledVehicleMovementComponent* Movement(const UActorComponent* C)
{
	return C->GetOwner() ? C->GetOwner()->FindComponentByClass<UChaosWheeledVehicleMovementComponent>() : nullptr;
}

TSharedPtr<FJsonValue> UMusubiVehicleHandover::CaptureHandoverState()
{
	UChaosWheeledVehicleMovementComponent* M = Movement(this);
	if (!M) return nullptr;
	const FWheeledSnaphotData S = M->GetSnapshot();
	TSharedPtr<FJsonObject> O = MakeShared<FJsonObject>();
	O->SetNumberField(TEXT("gear"), S.SelectedGear);
	O->SetNumberField(TEXT("rpm"), FMath::RoundToDouble(S.EngineRPM * 10.0) / 10.0);
	TArray<TSharedPtr<FJsonValue>> Wheels;
	for (const FWheelSnapshot& W : S.WheelSnapshots)
	{
		TArray<TSharedPtr<FJsonValue>> R;
		for (double D : { (double)W.SuspensionOffset, (double)W.WheelRotationAngle, (double)W.SteeringAngle, (double)W.WheelRadius, (double)W.WheelAngularVelocity })
			R.Add(MakeShared<FJsonValueNumber>(FMath::RoundToDouble(D * 100.0) / 100.0));
		Wheels.Add(MakeShared<FJsonValueArray>(R));
	}
	O->SetArrayField(TEXT("wheels"), Wheels);
	return MakeShared<FJsonValueObject>(O);
}

void UMusubiVehicleHandover::RestoreHandoverState(const TSharedPtr<FJsonValue>& State)
{
	UChaosWheeledVehicleMovementComponent* M = Movement(this);
	const TSharedPtr<FJsonObject>* O = nullptr;
	if (!M || !State.IsValid() || !State->TryGetObject(O) || !O) return;
	// the root body (pose, velocities) was restored by the authority component; keep it and set the drivetrain/wheels
	FWheeledSnaphotData S = M->GetSnapshot();
	double D;
	if ((*O)->TryGetNumberField(TEXT("gear"), D)) S.SelectedGear = (int32)D;
	if ((*O)->TryGetNumberField(TEXT("rpm"), D)) S.EngineRPM = (float)D;
	const TArray<TSharedPtr<FJsonValue>>* Wheels = nullptr;
	if ((*O)->TryGetArrayField(TEXT("wheels"), Wheels) && Wheels)
		for (int32 i = 0; i < Wheels->Num() && i < S.WheelSnapshots.Num(); ++i)
		{
			const TArray<TSharedPtr<FJsonValue>>* R = nullptr;
			if (!(*Wheels)[i].IsValid() || !(*Wheels)[i]->TryGetArray(R) || !R || R->Num() < 5) continue;
			FWheelSnapshot& W = S.WheelSnapshots[i];
			W.SuspensionOffset = (*R)[0]->AsNumber();
			W.WheelRotationAngle = (*R)[1]->AsNumber();
			W.SteeringAngle = (*R)[2]->AsNumber();
			W.WheelRadius = (*R)[3]->AsNumber();
			W.WheelAngularVelocity = (*R)[4]->AsNumber();
		}
	M->SetSnapshot(S);
	UE_LOG(LogMusubiServer, Log, TEXT("%s: vehicle snapshot restored (gear %d, %d wheels)"), *GetOwner()->GetName(), S.SelectedGear, S.WheelSnapshots.Num());
}
