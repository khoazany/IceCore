use std::rc::Rc;
use std::cell::{Cell, RefCell};
use std::ops::Deref;
use std::time::SystemTime;
use std::collections::BTreeMap;

use chrono;

use wasm_core::jit::compiler::{Compiler, ExecutionContext};
use wasm_core::jit::runtime::RuntimeConfig;
use wasm_core::module::Module;
use container::Container;

use super::resolver::{RcLssaResolver, LssaResolver, NullResolver};
use super::stats::AppStats;
use super::namespace::Migration;
use config::AppPermission;

// `inner` is intended to be used internally only and this should NOT be `Clone`.
pub struct Application {
    inner: Rc<ApplicationImpl>
}

pub struct ApplicationImpl {
    pub(super) name: String,
    pub(super) config: AppConfig,

    pub(super) code: Vec<u8>,
    code_sha256: [u8; 32],

    currently_inside: Cell<usize>,
    module: Module,
    execution: ExecutionContext,

    start_time: SystemTime,
    resolvers: RefCell<BTreeMap<String, RcLssaResolver>>,

    invoke0_fn: extern "C" fn (i64) -> i64,
    invoke1_fn: extern "C" fn (i64, i64) -> i64,
    invoke2_fn: extern "C" fn (i64, i64, i64) -> i64,
    invoke3_fn: extern "C" fn (i64, i64, i64, i64) -> i64,
    invoke4_fn: extern "C" fn (i64, i64, i64, i64, i64) -> i64,
    pub(super) container: Container
}

struct AppInsideHandle<'a> {
    app: &'a ApplicationImpl
}

impl<'a> AppInsideHandle<'a> {
    fn new(app: &'a ApplicationImpl) -> AppInsideHandle<'a> {
        let v = app.currently_inside.get() + 1;
        app.currently_inside.set(v);

        AppInsideHandle {
            app: app
        }
    }
}

impl<'a> Drop for AppInsideHandle<'a> {
    fn drop(&mut self) {
        let v = self.app.currently_inside.get() - 1;
        self.app.currently_inside.set(v);
    }
}

pub type AppConfig = ::config::ApplicationConfig;

impl Deref for Application {
    type Target = ApplicationImpl;

    fn deref(&self) -> &Self::Target {
        &*self.inner
    }
}

impl Application {
    pub fn new(
        m: Module,
        code: &[u8],
        sha256: [u8; 32],
        config: AppConfig,
        container: Container
    ) -> Application {
        let mut rt_config = RuntimeConfig::default();

        rt_config.mem_default = config.memory.min;
        rt_config.mem_max = config.memory.max;
        rt_config.opt_level = if config.optimize {
            dwarning!(
                logger!(&config.name),
                "Optimization is slow and may take a few minutes."
            );

            1
        } else {
            0
        };

        let compiler = Compiler::with_runtime_config(&m, rt_config).unwrap();

        let vm = compiler.compile().unwrap().into_execution_context();

        let invoke0 = unsafe { vm.get_function_checked(
            m.lookup_exported_func("__app_invoke0").unwrap()
        ) };
        let invoke1 = unsafe { vm.get_function_checked(
            m.lookup_exported_func("__app_invoke1").unwrap()
        ) };
        let invoke2 = unsafe { vm.get_function_checked(
            m.lookup_exported_func("__app_invoke2").unwrap()
        ) };
        let invoke3 = unsafe { vm.get_function_checked(
            m.lookup_exported_func("__app_invoke3").unwrap()
        ) };
        let invoke4 = unsafe { vm.get_function_checked(
            m.lookup_exported_func("__app_invoke4").unwrap()
        ) };

        let name = config.name.clone();

        let app = Rc::new(ApplicationImpl {
            name: name,
            config: config,
            code: code.to_vec(),
            code_sha256: sha256,
            currently_inside: Cell::new(0),
            module: m,
            execution: vm,
            start_time: SystemTime::now(),
            resolvers: RefCell::new(BTreeMap::new()),
            invoke0_fn: invoke0,
            invoke1_fn: invoke1,
            invoke2_fn: invoke2,
            invoke3_fn: invoke3,
            invoke4_fn: invoke4,
            container: container
        });

        let mut cwa_resolver = LssaResolver::new(Rc::downgrade(&app), "cwa", "", NullResolver::new());
        cwa_resolver.init_cwa_namespaces();
        let cwa_resolver = RcLssaResolver::from(cwa_resolver);

        let mut resolver = LssaResolver::new(Rc::downgrade(&app), "env", "__ice_", cwa_resolver.clone());
        resolver.init_ice_namespaces();
        let resolver = RcLssaResolver::from(resolver);

        app.resolvers.borrow_mut().insert("cwa".into(), cwa_resolver);
        app.resolvers.borrow_mut().insert("ice".into(), resolver.clone());

        app.execution.set_native_resolver(resolver);

        Application {
            inner: app
        }
    }

    pub fn initialize(&self, initializer_name: Option<&str>) {
        let _inside = AppInsideHandle::new(self);

        let initializer_name = initializer_name.unwrap_or("__app_init");

        let entry_id = match self.module.lookup_exported_func(initializer_name) {
            Some(v) => v,
            None => return
        };

        let entry: extern "C" fn () -> i64 = unsafe {
            self.execution.get_function_checked(entry_id)
        };

        let ret = self.execution.rt.protected_call(|| entry());
        if ret != 0 {
            panic!("initialize: Initializer reported failure");
        }
    }

    pub fn stats(&self) -> AppStats {
        let dt: chrono::DateTime<chrono::Utc> = chrono::DateTime::from(self.start_time);
        let diff: chrono::Duration = chrono::Duration::from_std(
            SystemTime::now().duration_since(self.start_time).unwrap()
        ).unwrap();
        AppStats {
            start_time: dt.timestamp_millis(),
            running_time: diff.num_milliseconds()
        }
    }
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct AppMigration {
    pub code_sha256: [u8; 32],
    pub memory: Vec<u8>,
    pub globals: Vec<i64>,
    pub modules: BTreeMap<String, ModuleMigration>
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct ModuleMigration {
    pub namespaces: BTreeMap<String, Migration>
}

impl ApplicationImpl {
    pub fn check_permission(&self, perm: &AppPermission) -> Result<(), ()> {
        let id = self.container.lookup_app_id_by_name(&self.name).unwrap();

        let cs = self.container.config_state.read().unwrap();
        let app_config = &cs.config.applications[id];

        if !app_config.metadata.permissions.contains(perm) {
            Err(())
        } else {
            Ok(())
        }
    }

    #[allow(dead_code)]
    pub fn id(&self) -> usize {
        self.container.lookup_app_id_by_name(&self.name).unwrap()
    }

    pub fn start_migration(&self) -> AppMigration {
        let resolvers = self.resolvers.borrow();
        let mut mig = AppMigration::default();
        for (k, r) in &*resolvers {
            let mut mm = ModuleMigration::default();
            for (name, ns) in r.inner.get_namespaces() {
                mm.namespaces.insert(
                    name.clone(),
                    ns.start_migration().unwrap_or_else(|| {
                        panic!("Unable to migrate namespace {}", name)
                    })
                );
            }
            mig.modules.insert(k.clone(), mm);
        }

        let rt = &self.execution.rt;
        mig.memory = unsafe { &*rt.get_memory() }.to_vec();
        mig.globals = unsafe {
            ::std::slice::from_raw_parts(
                (&*rt.get_jit_info()).global_begin,
                rt.source_module.globals.len()
            )
        }.to_vec();
        mig.code_sha256 = self.code_sha256;

        mig
    }

    pub fn complete_migration(&self, mig: &AppMigration) {
        if mig.code_sha256 != self.code_sha256 {
            panic!("Checksum mismatch");
        }

        let rt = &self.execution.rt;
        let mem_len = unsafe { &*rt.get_memory() }.len();
        if mem_len < mig.memory.len() {
            rt.grow_memory(mig.memory.len() - mem_len);
        }
        let mem = unsafe { &mut *rt.get_memory_mut() };
        mem[0..mig.memory.len()].copy_from_slice(&mig.memory);

        if rt.source_module.globals.len() != mig.globals.len() {
            panic!("Global len mismatch");
        }

        let globals = unsafe { ::std::slice::from_raw_parts_mut(
            (&*rt.get_jit_info()).global_begin,
            rt.source_module.globals.len()
        ) };
        globals.copy_from_slice(&mig.globals);

        let resolvers = self.resolvers.borrow();
        for (k, r) in &*resolvers {
            let mm = mig.modules.get(k).unwrap_or_else(|| {
                panic!("Migration data not found for module {}", k);
            });
            for (name, ns) in r.inner.get_namespaces() {
                let ns_data = mm.namespaces.get(name).unwrap_or_else(|| {
                    panic!("Migration data not found for namespace {}", name);
                });
                ns.complete_migration(ns_data);
            }
        }
    }

    #[allow(dead_code)]
    pub fn invoke0(&self, target: i32) -> i32 {
        self.execution.rt.protected_call(|| {
            (self.invoke0_fn)((target as u32) as _) as _
        })
    }

    #[allow(dead_code)]
    pub fn invoke1(
        &self,
        target: i32,
        arg1: i32
    ) -> i32 {
        self.execution.rt.protected_call(|| {
            (self.invoke1_fn)(
                (target as u32) as _,
                (arg1 as u32) as _
            ) as _
        })
    }

    #[allow(dead_code)]
    pub fn invoke2(
        &self,
        target: i32,
        arg1: i32,
        arg2: i32
    ) -> i32 {
        self.execution.rt.protected_call(|| {
            (self.invoke2_fn)(
                (target as u32) as _,
                (arg1 as u32) as _,
                (arg2 as u32) as _
            ) as _
        })
    }

    #[allow(dead_code)]
    pub fn invoke3(
        &self,
        target: i32,
        arg1: i32,
        arg2: i32,
        arg3: i32
    ) -> i32 {
        self.execution.rt.protected_call(|| {
            (self.invoke3_fn)(
                (target as u32) as _,
                (arg1 as u32) as _,
                (arg2 as u32) as _,
                (arg3 as u32) as _
            ) as _
        })
    }

    #[allow(dead_code)]
    pub fn invoke4(
        &self,
        target: i32,
        arg1: i32,
        arg2: i32,
        arg3: i32,
        arg4: i32
    ) -> i32 {
        self.execution.rt.protected_call(|| {
            (self.invoke4_fn)(
                (target as u32) as _,
                (arg1 as u32) as _,
                (arg2 as u32) as _,
                (arg3 as u32) as _,
                (arg4 as u32) as _
            ) as _
        })
    }
}
