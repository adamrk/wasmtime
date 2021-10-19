# Existing setup

```
 DEBUG wasmtime_cranelift::compiler > FuncIndex(0) translated in 1.950180269s
 TRACE wasmtime_cranelift::compiler > FuncIndex(0) timing info
======== ========  ==================================
   Total     Self  Pass
-------- --------  ----------------------------------
   0.014    0.014  Translate WASM function
   1.936    0.017  Compilation passes
   0.008    0.008  Control flow graph
   0.007    0.007  Dominator tree
   0.001    0.001  Loop analysis
   0.001    0.001  Pre-legalization rewriting
   0.000    0.000  Dead code elimination
   0.001    0.001  Global value numbering
   0.004    0.000  Loop invariant code motion
   0.000    0.000  Remove unreachable blocks
   0.001    0.001  Remove constant phi-nodes
   0.014    0.014  VCode lowering
   0.002    0.002  VCode post-register allocation finalization
   0.621    0.621  VCode emission
   0.000    0.000  VCode emission finalization
   1.264    1.264  Register allocation
   0.000    0.000  Binary machine code emission
======== ========  ==================================
```

# With regallor change

```
 DEBUG wasmtime_cranelift::compiler > FuncIndex(0) translated in 632.089765ms
 TRACE wasmtime_cranelift::compiler > FuncIndex(0) timing info
======== ========  ==================================
   Total     Self  Pass
-------- --------  ----------------------------------
   0.013    0.013  Translate WASM function
   0.619    0.015  Compilation passes
   0.007    0.007  Control flow graph
   0.008    0.008  Dominator tree
   0.001    0.001  Loop analysis
   0.001    0.001  Pre-legalization rewriting
   0.000    0.000  Dead code elimination
   0.001    0.001  Global value numbering
   0.005    0.000  Loop invariant code motion
   0.000    0.000  Remove unreachable blocks
   0.001    0.001  Remove constant phi-nodes
   0.011    0.011  VCode lowering
   0.002    0.002  VCode post-register allocation finalization
   0.554    0.554  VCode emission
   0.000    0.000  VCode emission finalization
   0.018    0.018  Register allocation
   0.000    0.000  Binary machine code emission
======== ========  ==================================
```