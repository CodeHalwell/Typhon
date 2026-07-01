cat << 'INNER_EOF' > patch2.diff
--- tyc/crates/tyc-types/src/lib.rs
+++ tyc/crates/tyc-types/src/lib.rs
@@ -6540,7 +6540,7 @@
             }
         }
     }
-    let mut v = V {
+    let mut v = V::<'a> {
         names: std::collections::HashSet::new(),
     };
     v.visit_expr(expr);
INNER_EOF
patch tyc/crates/tyc-types/src/lib.rs patch2.diff
