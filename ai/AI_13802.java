import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_13802 extends AI {
public class u_Test extends NativeObjectLeekValue {
public u_Test() throws LeekRunException {
allocateRAM(this, 0);
}
public u_Test(u_Test o, int level) throws LeekRunException {
}
public Object init() throws LeekRunException {
return null;
}
public double u_getReal() throws LeekRunException {
return 1.5;
}
public Object u_setInteger() throws LeekRunException {
final Long u_a = (Long) null;
ops(0);return u_setInteger(u_a);
}
public Object u_setInteger(Long u_a) throws LeekRunException {
return null;
}
}
public ClassLeekValue u_Test = new ClassLeekValue(this, "Test", null, u_Test.class);
public u_Test new_u_Test(Object... args) throws LeekRunException {
return (u_Test) execute(u_Test, args);
}
public AI_13802() throws LeekRunException {
super(2, 3);
u_Test.initFields = new FunctionLeekValue(0) {public Object run(AI ai, Object u_this, Object... values) throws LeekRunException {
return null;
}};
u_Test.addMethod("getReal", 0, new FunctionLeekValue(0) { public Object run(AI ai, Object thiz, Object... args) throws LeekRunException {
return ((u_Test) thiz).u_getReal(); }}, AccessLevel.PUBLIC);
u_Test.addGenericMethod("getReal");
u_Test.addMethod("setInteger", 0, new FunctionLeekValue(0) { public Object run(AI ai, Object thiz, Object... args) throws LeekRunException {
return ((u_Test) thiz).u_setInteger(); }}, AccessLevel.PUBLIC);
u_Test.addMethod("setInteger", 1, new FunctionLeekValue(0) { public Object run(AI ai, Object thiz, Object... args) throws LeekRunException {
return ((u_Test) thiz).u_setInteger((Long) args[0]); }}, AccessLevel.PUBLIC);
u_Test.addGenericMethod("setInteger");
}
private void createStaticClass_Test() throws LeekRunException {
}
private void initClass_Test() throws LeekRunException {
}
public void staticInit() throws LeekRunException {
createStaticClass_Test();
initClass_Test();
}
public Object runIA(Session session) throws LeekRunException {
u_Test u_test = (u_Test) ops(new_u_Test(), 1);
return ops(u_test.u_setInteger((long)(double) u_test.u_getReal()), 2);
}
protected String getAIString() { return "<snippet 13802>";}
protected String[] getErrorFiles() { return new String[] {"<snippet 13802>", };}

protected int[] getErrorFilesID() { return new int[] {13802, };}

}
