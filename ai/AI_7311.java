import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_7311 extends AI {
public class u_C extends NativeObjectLeekValue {
public u_C() throws LeekRunException {
allocateRAM(this, 0);
}
public u_C(u_C o, int level) throws LeekRunException {
}
public Object init() throws LeekRunException {
return null;
}
}
public ClassLeekValue u_C = new ClassLeekValue(this, "C", null, u_C.class);
public u_C new_u_C(Object... args) throws LeekRunException {
return (u_C) execute(u_C, args);
}
private final LegacyArrayLeekValue u_C_execute_2(LegacyArrayLeekValue u_items, long p_factor) throws LeekRunException {
final var u_factor = new Box<Long>(AI_7311.this, p_factor);
Object u_arr = ops(u_items, 1);
Object u_f = ops(new FunctionLeekValue(0) {public Object run(AI ai, Object thiz, Object... values) throws LeekRunException {
ops(1);return u_factor.get();
}}, 1);
ops(u_arr = Array_arraySort_af(u_arr, new FunctionLeekValue(2) {public Object run(AI ai, Object thiz, Object... values) throws LeekRunException {var u_a = (values.length > 0 ?  values[0] : null);var u_b = (values.length > 1 ?  values[1] : null);
ops(1);ops(3); return (Object) mul(((Object) sub(u_a, u_b)), u_factor.get());
}}), 1);
return toLegacyArray(0, u_arr);
}
public AI_7311() throws LeekRunException {
super(1, 3);
u_C.initFields = new FunctionLeekValue(0) {public Object run(AI ai, Object u_this, Object... values) throws LeekRunException {
return null;
}};
u_C.addStaticMethod("execute", 2, new FunctionLeekValue(1) { public Object run(AI ai, Object thiz, Object... args) throws LeekRunException { return u_C_execute_2((LegacyArrayLeekValue) args[0], (Long) args[1]); }}, AccessLevel.PUBLIC);
u_C.addGenericStaticMethod("execute");
}
private void createStaticClass_C() throws LeekRunException {
}
private void initClass_C() throws LeekRunException {
}
public void staticInit() throws LeekRunException {
createStaticClass_C();
initClass_C();
}
public Object runIA(Session session) throws LeekRunException {
ops(7); return u_C_execute_2(new LegacyArrayLeekValue(AI_7311.this, new Object[] { 3l, 1l, 2l }, false), 1l);
}
protected String getAIString() { return "<snippet 7311>";}
protected String[] getErrorFiles() { return new String[] {"<snippet 7311>", };}

protected int[] getErrorFilesID() { return new int[] {7311, };}

private LegacyArrayLeekValue Array_arraySort_af(Object a0, Object a1) throws LeekRunException {
LegacyArrayLeekValue x0; try { x0 = toLegacyArray(1, a0); } catch (ClassCastException e) { return new LegacyArrayLeekValue(AI_7311.this); }
FunctionLeekValue<Number> x1; try { x1 = toFunction(2, a1); } catch (ClassCastException e) { return new LegacyArrayLeekValue(AI_7311.this); }
return x0.arraySort_v1_3(this, x1);
}

}
