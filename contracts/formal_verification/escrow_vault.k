// Formal Semantics of Soroban Escrow Vault in K-Framework
module ESCROW-VAULT-SYNTAX
    imports DOMAIN-VALUE

    syntax StateKey ::= "TotalDeposited" | "TotalLocked" | "TotalClaimed" | "ReentrancyLock"
    syntax Command  ::= "deposit" "(" Int ")"
                      | "lock" "(" Int ")"
                      | "claim" "(" Int ")"
                      | "refund" "(" Int ")"
                      | "reclaim" "(" Int ")"
endmodule

module ESCROW-VAULT
    imports ESCROW-VAULT-SYNTAX
    imports INT
    imports BOOL

    configuration
        <k> $PGM:K </k>
        <state>
            <totalDeposited> 0:Int </totalDeposited>
            <totalLocked> 0:Int </totalLocked>
            <totalClaimed> 0:Int </totalClaimed>
            <reentrancyLock> 0:Int </reentrancyLock>
        </state>

    // Rule 1: Deposit
    rule <k> deposit(N:Int) => . ... </k>
         <state>
             <totalDeposited> D:Int => D +Int N </totalDeposited>
             <reentrancyLock> 0 </reentrancyLock>
             ...
         </state>
         requires N >Int 0

    // Rule 2: Lock
    rule <k> lock(N:Int) => . ... </k>
         <state>
             <totalDeposited> D:Int </totalDeposited>
             <totalLocked> L:Int => L +Int N </totalLocked>
             <totalClaimed> C:Int </totalClaimed>
             <reentrancyLock> 0 </reentrancyLock>
             ...
         </state>
         requires N >Int 0 andBool (L +Int N +Int C <=Int D)

    // Rule 3: Claim
    rule <k> claim(N:Int) => . ... </k>
         <state>
             <totalLocked> L:Int => L -Int N </totalLocked>
             <totalClaimed> C:Int => C +Int N </totalClaimed>
             <reentrancyLock> 0 </reentrancyLock>
             ...
         </state>
         requires N >Int 0 andBool L >=Int N

    // Mathematical Invariant Assertion: totalDeposited >= totalLocked + totalClaimed
    syntax Bool ::= checkSolvencyInvariant(Int, Int, Int) [function]
    rule checkSolvencyInvariant(D:Int, L:Int, C:Int) => (D >=Int (L +Int C)) andBool (D >=Int 0) andBool (L >=Int 0) andBool (C >=Int 0)
endmodule
