cat << 'INNER_EOF' > patch3.diff
--- tyc/crates/tyc-types/src/lib.rs
+++ tyc/crates/tyc-types/src/lib.rs
@@ -5929,7 +5929,7 @@
                         // partial source — the assignment is a merge.
                     } else {
                         for n in collect_names_in_expr(other) {
-                            tracked.remove(&n);
+                            tracked.remove(n);
                         }
                     }
                 }
@@ -6077,12 +6077,12 @@
     // Any tracked name referenced in the arguments is dropped.
     for arg in &call.arguments.args {
         for n in collect_names_in_expr(arg) {
-            tracked.remove(&n);
+            tracked.remove(n);
         }
     }
     for kw in &call.arguments.keywords {
         for n in collect_names_in_expr(&kw.value) {
-            tracked.remove(&n);
+            tracked.remove(n);
         }
     }
 }
@@ -6118,7 +6118,7 @@
         return;
     }
     for n in collect_names_in_expr(value) {
-        tracked.remove(&n);
+        tracked.remove(n);
     }
 }

@@ -6372,10 +6372,10 @@
         .iter()
         .filter_map(|n| {
             c.uninit_instances
-                .get(n)
+                .get(*n)
                 .filter(|info| !info.missing.is_empty())
-                .map(|info| (n.clone(), info.clone()))
+                .map(|info| (n.to_string(), info.clone()))
         })
         .collect();
     for (binding, info) in snapshot {
@@ -9779,7 +9779,7 @@
                 let names_in_value = collect_names_in_expr(value);
                 if names_in_value
                     .iter()
-                    .any(|n| c.uninit_instances.contains_key(n))
+                    .any(|n| c.uninit_instances.contains_key(*n))
                 {
                     audit_check_escape(c, value);
                 }
INNER_EOF
patch tyc/crates/tyc-types/src/lib.rs patch3.diff
