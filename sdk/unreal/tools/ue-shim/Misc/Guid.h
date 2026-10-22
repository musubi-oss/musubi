#pragma once
#include "CoreMinimal.h"
struct FGuid { uint32 A = 0, B = 0, C = 0, D = 0; static FGuid NewGuid() { FGuid G; G.A = (uint32)std::rand(); G.B = (uint32)std::rand(); G.C = (uint32)std::rand(); G.D = (uint32)std::rand(); return G; } };
