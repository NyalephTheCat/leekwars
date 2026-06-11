import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_5892202 extends AI {
public class u_MultiFunction extends NativeObjectLeekValue {
@Private public double val;
public u_MultiFunction() throws LeekRunException {
allocateRAM(this, 2);
}
public u_MultiFunction(u_MultiFunction o, int level) throws LeekRunException {
this.val = level == 1 ? o.val : (double) copy(o.val, level - 1);
}
private Object u_MultiFunction$body_1(double u_v) throws LeekRunException {
ops(val = u_v, 2);
return null;
}
public Object init() throws LeekRunException {
return null;
}
public Object init(double u_v) throws LeekRunException {
return u_MultiFunction$body_1(u_v);
}
public boolean u_isNear(double u_x) throws LeekRunException {
ops(5); return less(NumberClass.abs(AI_5892202.this, val - u_x), g_EPSILON);
}
}
public ClassLeekValue u_MultiFunction = new ClassLeekValue(this, "MultiFunction", null, u_MultiFunction.class);
public u_MultiFunction new_u_MultiFunction(Object... args) throws LeekRunException {
return (u_MultiFunction) execute(u_MultiFunction, args);
}
public AI_5892202() throws LeekRunException {
super(3, 4);
u_MultiFunction.initFields = new FunctionLeekValue(0) {public Object run(AI ai, Object u_this, Object... values) throws LeekRunException {
return null;
}};
u_MultiFunction.addMethod("isNear", 1, new FunctionLeekValue(0) { public Object run(AI ai, Object thiz, Object... args) throws LeekRunException {
return ((u_MultiFunction) thiz).u_isNear((Double) args[0]); }}, AccessLevel.PUBLIC);
u_MultiFunction.addGenericMethod("isNear");
}
private void createStaticClass_MultiFunction() throws LeekRunException {
}
private void initClass_MultiFunction() throws LeekRunException {
}
public void staticInit() throws LeekRunException {
createStaticClass_MultiFunction();
initClass_MultiFunction();
}
private double g_EPSILON = 0.0;
private boolean g_init_EPSILON = false;
public Object runIA(Session session) throws LeekRunException {
if (!g_init_EPSILON) { g_EPSILON = (double) 1.0E-9; g_init_EPSILON = true; ops(1); }
u_MultiFunction u_mf = (u_MultiFunction) ops(new_u_MultiFunction(1.0), 1);
ops(2); return u_mf.u_isNear(1.0) ? 1l : 0l;
}
protected String getAIString() { return "Main_1a99d3487b276_29";}
protected String[] getErrorFiles() { return new String[] {"multi-function", "util", "Main_1a99d3487b276_29", };}

protected int[] getErrorFilesID() { return new int[] {140113325, 3601347, 5892202, };}

}
